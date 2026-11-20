//! Docked Browser — local web GUI server (Rust port of `gui/server.py`).
//!
//! Same HTTP API, same files on disk, same `bin/docked-browser` calls:
//!
//! ```text
//! GET  /                    → HTML UI (gui/templates/index.html)
//! GET  /api/status          → {image_built, containers[], profiles[], icons{}, for_now, chrome_update, theme…}
//! GET  /api/stats           → {containers:[{profile,cpu_pct,mem_used,mem_limit,mem_pct,state}]}
//! GET  /api/prefs           → {theme, theme_resolved, advanced, offload_after_min}
//! POST /api/prefs           → {theme?, advanced?, offload_after_min?}
//! GET  /api/icon/<p>        → custom PNG bytes (404 if none)
//! POST /api/action          → {action, profile?}
//! POST /api/icon            → {profile, action: set|clear, image?: data-URL}
//! GET  /api/predict-sleep   → {enabled, updates, profiles{}, log[]}
//! POST /api/predict-sleep   → {enabled?: bool}
//! GET  /api/chrome-update   → cached update state (checks when due)
//! POST /api/chrome-update   → forced check
//! ```
//!
//! Reads and changes to the predict-sleep toggle are handled here.
//! The predict-sleep daemon, which `server.py` ran as a thread,
//! runs as a child (`bin/docked-predict-sleep`) when its lock is free; its
//! log lines are kept for `/api/predict-sleep`. Chrome update checks and icon
//! uploads are handled here.

mod habit;
mod offload;
mod profile_icon;
mod sleep_state;
mod scheduler;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{Map, Value, json};

// --- Constants ---------------------------------------------------------------
const IMAGE_NAME: &str = "local-docked-browser";
const LEGACY_IMAGE_NAME: &str = "local-chrome-cockpit";
const MAX_ICON_BYTES: usize = 2 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024; // 2 MB icon as base64 + slack
const MAX_HEAD_BYTES: usize = 64 * 1024;
const OFFLOAD_MAX_MIN: i64 = 7 * 24 * 60;
const DEFAULT_OFFLOAD_MIN: i64 = 120;
const FORCED_WAKE_WINDOW_S: f64 = 600.0;
const FEATURE_LAYOUT: i64 = 2;
const FEATURE_DIM: i64 = 16;
const MYT_OFFSET_SECS: i64 = 8 * 3600;
const WINDOW_WEEKS: i64 = 4;
const MIN_WEEKS: i64 = 3;
const CHECK_INTERVAL_S: f64 = 86400.0;
const RETRY_AFTER_FAIL_S: f64 = 3600.0;

const ALLOWED_ACTIONS: &[&str] = &[
    "build",
    "run",
    "activate",
    "pause",
    "resume",
    "stop",
    "delete",
    "pause-all",
    "resume-all",
    "stop-all",
    "offload-all",
];
const FLEET_ACTIONS: &[&str] = &["pause-all", "resume-all", "stop-all", "offload-all"];

const STATIC_FILES: &[(&str, &str, &str)] = &[
    ("/favicon.svg", "favicon.svg", "image/svg+xml"),
    ("/favicon.ico", "favicon.ico", "image/x-icon"),
    ("/favicon-16.png", "favicon-16.png", "image/png"),
    ("/favicon-32.png", "favicon-32.png", "image/png"),
    ("/apple-touch-icon.png", "apple-touch-icon.png", "image/png"),
];

// --- Paths -------------------------------------------------------------------
struct Paths {
    root: PathBuf,
    gui: PathBuf,
    script: PathBuf,
    template: PathBuf,
    static_dir: PathBuf,
    profiles_dir: PathBuf,
    state_dir: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();
static PREDICT_LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

fn paths() -> &'static Paths {
    PATHS.get().expect("paths initialised in main")
}

fn find_root() -> Option<PathBuf> {
    let is_root = |p: &Path| p.join("bin/docked-browser").is_file() && p.join("gui/templates").is_dir();
    if let Ok(dir) = std::env::var("DOCKED_BROWSER_ROOT") {
        let p = PathBuf::from(dir);
        if is_root(&p) {
            return Some(p);
        }
    }
    let starts = [
        std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()),
        std::env::current_dir().ok(),
    ];
    for start in starts.into_iter().flatten() {
        let mut cur: Option<&Path> = Some(&start);
        while let Some(p) = cur {
            if is_root(p) {
                return Some(p.to_path_buf());
            }
            cur = p.parent();
        }
    }
    None
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn init_paths(root: PathBuf) -> Paths {
    let gui = root.join("gui");
    let state_dir = match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(".local/share"),
    }
    .join("docked-browser");
    Paths {
        script: root.join("bin/docked-browser"),
        template: gui.join("templates/index.html"),
        static_dir: gui.join("static"),
        profiles_dir: home().join(".config/docker-chrome-profiles"),
        state_dir,
        gui,
        root,
    }
}

// --- Small helpers -----------------------------------------------------------
fn profile_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn now_f64() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Python truthiness for a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `int(value)` for JSON input.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Python `str(value)` for a JSON payload field.
fn py_str(v: Option<&Value>, default: &str) -> String {
    match v {
        None => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn session_wants_x11(env: &HashMap<String, String>) -> bool {
    match env.get("XDG_SESSION_TYPE").map(String::as_str) {
        Some("x11") => return true,
        Some("wayland") => return false,
        _ => {}
    }
    let desktop = format!(
        "{}:{}",
        env.get("XDG_CURRENT_DESKTOP").map(String::as_str).unwrap_or(""),
        env.get("DESKTOP_SESSION").map(String::as_str).unwrap_or("")
    )
    .to_lowercase();
    ["cinnamon", "xfce", "mate"].iter().any(|n| desktop.contains(n))
}

/// Env so Docked Browser can reach the active Wayland/X11 session.
fn desktop_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    let uid = unsafe { libc::getuid() };
    let runtime = env
        .get("XDG_RUNTIME_DIR")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("/run/user/{uid}"));
    env.entry("XDG_RUNTIME_DIR".into()).or_insert_with(|| runtime.clone());
    env.entry("DISPLAY".into()).or_insert_with(|| ":0".into());
    if session_wants_x11(&env) {
        env.remove("WAYLAND_DISPLAY");
    } else if env.get("WAYLAND_DISPLAY").is_none_or(|s| s.is_empty())
        && Path::new(&runtime).join("wayland-0").exists()
    {
        env.insert("WAYLAND_DISPLAY".into(), "wayland-0".into());
    }
    env
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

enum RunError {
    Timeout,
    Os(String),
}

/// `subprocess.run(..., capture_output=True, timeout=...)`; kills on timeout.
fn run_capture(mut cmd: Command, timeout: Duration, stdin_data: Option<Vec<u8>>) -> Result<Output, RunError> {
    cmd.stdin(if stdin_data.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| RunError::Os(e.to_string()))?;
    let feeder = stdin_data.and_then(|data| {
        let mut stdin = child.stdin.take()?;
        Some(thread::spawn(move || {
            let _ = stdin.write_all(&data);
        }))
    });
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = pipe(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err_t = pipe(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    // Block on wait() in a helper thread; kill by pid if the deadline passes.
    let pid = child.id() as libc::pid_t;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = match rx.recv_timeout(timeout) {
        Ok(Ok(st)) => st,
        Ok(Err(e)) => return Err(RunError::Os(e.to_string())),
        Err(_) => {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            let _ = rx.recv();
            return Err(RunError::Timeout);
        }
    };
    if let Some(f) = feeder {
        let _ = f.join();
    }
    let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    Ok(Output { code: status.code(), stdout, stderr })
}

fn simple_cmd(args: &[&str], timeout_s: u64) -> Option<Output> {
    let mut cmd = Command::new(args[0]);
    cmd.args(&args[1..]);
    run_capture(cmd, Duration::from_secs(timeout_s), None).ok()
}

// --- bin/docked-browser --------------------------------------------------------
fn result(ok: bool, output: impl Into<String>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("ok".into(), Value::Bool(ok));
    m.insert("output".into(), Value::String(output.into()));
    m
}

/// Run `bin/docked-browser <action> [profile]` and return {ok, output}.
fn run_cockpit(action: &str, profile: Option<&str>) -> Map<String, Value> {
    let p = paths();
    if !p.script.is_file() {
        return result(false, format!("Missing script: {}", p.script.display()));
    }
    let mut argv = vec![p.script.to_string_lossy().into_owned(), action.to_string()];
    if let Some(prof) = profile {
        if !profile_ok(prof) {
            return result(false, "Invalid profile name. Use letters, numbers, _ or - (max 64).");
        }
        argv.push(prof.to_string());
    }
    let timeout = match action {
        "build" => 900,
        "offload-all" => 600, // pages every paused profile out to disk
        _ => 60,
    };
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&p.root).env_clear().envs(desktop_env());
    let out = match run_capture(cmd, Duration::from_secs(timeout), None) {
        Ok(o) => o,
        Err(RunError::Timeout) => return result(false, format!("Timed out running: {}", argv.join(" "))),
        Err(RunError::Os(e)) => return result(false, e),
    };
    let ok = out.code == Some(0);
    let text = format!("{}{}", out.stdout, out.stderr);
    let text = text.trim();
    result(ok, if text.is_empty() { "(no output)" } else { text })
}

// --- Docker status / profiles ------------------------------------------------
fn docker_chrome_status() -> Vec<Map<String, Value>> {
    let Some(out) = simple_cmd(
        &["docker", "ps", "-a", "--filter", "name=chrome-", "--format", "{{.Names}}\t{{.Status}}\t{{.State}}"],
        15,
    ) else {
        return vec![];
    };
    let mut rows = vec![];
    for line in out.stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 || !parts[0].starts_with("chrome-") {
            continue;
        }
        let (name, status, state) = (parts[0], parts[1], parts[2]);
        let mut row = Map::new();
        row.insert("name".into(), json!(name));
        row.insert("profile".into(), json!(&name["chrome-".len()..]));
        row.insert("status".into(), json!(status));
        row.insert("state".into(), json!(state));
        row.insert("paused".into(), json!(state == "paused"));
        row.insert("running".into(), json!(state == "running"));
        rows.push(row);
    }
    rows
}

fn row_str<'a>(row: &'a Map<String, Value>, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn row_bool(row: &Map<String, Value>, key: &str) -> bool {
    row.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn known_profiles(rows: &[Map<String, Value>]) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    if let Ok(rd) = fs::read_dir(&paths().profiles_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() && profile_ok(&name) {
                set.insert(name);
            }
        }
    }
    for r in rows {
        set.insert(row_str(r, "profile").to_string());
    }
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

fn seed_chrome_fonts(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    fn section<'a>(parent: &'a mut Map<String, Value>, key: &str) -> std::io::Result<&'a mut Map<String, Value>> {
        parent.entry(key).or_insert_with(|| json!({})).as_object_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Chrome preference {key} must be an object"))
        })
    }

    let mut prefs = read_json(path).unwrap_or_else(|| json!({}));
    let data = prefs.as_object_mut().ok_or("Chrome Preferences must be an object")?;
    let webkit = section(data, "webkit")?;
    section(webkit, "webprefs")?.insert("fonts".into(), json!({
        "standard": {"Zyyy": "Inter"},
        "sansserif": {"Zyyy": "Inter"},
        "serif": {"Zyyy": "Noto Serif"},
        "fixed": {"Zyyy": "JetBrains Mono"},
    }));
    section(data, "browser")?.entry("has_seen_welcome_page").or_insert(json!(true));
    fs::write(path, serde_json::to_vec(&prefs)?)?;
    Ok(())
}

fn cli_status() -> Value {
    let rows = docker_chrome_status();
    let active = fs::read_to_string(paths().state_dir.join("active-profile")).unwrap_or_default();
    let active = active.trim();
    let active = if profile_ok(active) { active } else { "" };
    let mut names = known_profiles(&rows);
    names.retain(|name| profile_ok(name));
    names.sort_by_key(|name| name != active);
    let profiles: Vec<_> = names.iter().map(|name| {
        let state = rows.iter().find(|row| row_str(row, "profile") == name)
            .map(|row| row_str(row, "state"))
            .filter(|state| matches!(*state, "paused" | "running"))
            .unwrap_or("stopped");
        let installed = paths().state_dir.join("icons").join(format!("docked-browser-{name}.png"));
        let custom = icon_path(name);
        let icon = [installed, custom].into_iter().find(|path| path.is_file());
        json!({
            "profile": name,
            "state": state,
            "active": name == active,
            "icon": icon.as_deref().and_then(Path::to_str).unwrap_or_default(),
        })
    }).collect();
    json!({"active": active, "profiles": profiles})
}

fn image_built() -> bool {
    [IMAGE_NAME, LEGACY_IMAGE_NAME]
        .iter()
        .any(|img| simple_cmd(&["docker", "image", "inspect", img], 15).is_some_and(|o| o.code == Some(0)))
}

fn icon_path(profile: &str) -> PathBuf {
    paths().profiles_dir.join(profile).join("dock-icon.png")
}

// --- Prefs / theme (gui/control.py) ------------------------------------------
fn load_prefs() -> Map<String, Value> {
    let mut data = Map::new();
    data.insert("theme".into(), json!("system"));
    data.insert("advanced".into(), json!(false));
    data.insert("offload_after_min".into(), json!(DEFAULT_OFFLOAD_MIN));
    if let Some(Value::Object(raw)) = read_json(&paths().state_dir.join("prefs.json")) {
        for (k, v) in raw {
            data.insert(k, v);
        }
    }
    let theme = py_str(data.get("theme"), "system").to_lowercase();
    let theme = if matches!(theme.as_str(), "light" | "dark" | "system") { theme } else { "system".into() };
    data.insert("theme".into(), json!(theme));
    let adv = truthy(data.get("advanced"));
    data.insert("advanced".into(), json!(adv));
    let minutes = data.get("offload_after_min").and_then(py_int).unwrap_or(DEFAULT_OFFLOAD_MIN);
    data.insert("offload_after_min".into(), json!(minutes.clamp(0, OFFLOAD_MAX_MIN)));
    data
}

fn detect_system_theme() -> &'static str {
    if let Some(o) = simple_cmd(&["gsettings", "get", "org.gnome.desktop.interface", "color-scheme"], 2) {
        if o.code == Some(0) {
            match o.stdout.trim().trim_matches(|c| c == '\'' || c == '"') {
                "prefer-dark" => return "dark",
                "prefer-light" | "default" => return "light",
                _ => {}
            }
        }
    }
    if let Some(o) = simple_cmd(&["gsettings", "get", "org.gnome.desktop.interface", "gtk-theme"], 2) {
        if o.code == Some(0) && o.stdout.trim().to_lowercase().contains("dark") {
            return "dark";
        }
    }
    "light"
}

fn resolve_theme(pref: &str) -> String {
    match pref {
        "dark" | "light" => pref.to_string(),
        _ => detect_system_theme().to_string(),
    }
}

fn prefs_payload_from(data: &Map<String, Value>) -> Map<String, Value> {
    let pref = data.get("theme").and_then(Value::as_str).unwrap_or("system");
    let pref = if matches!(pref, "light" | "dark" | "system") { pref } else { "system" };
    let mut m = Map::new();
    m.insert("theme".into(), json!(pref));
    m.insert("theme_resolved".into(), json!(resolve_theme(pref)));
    m.insert("advanced".into(), json!(truthy(data.get("advanced"))));
    m.insert(
        "offload_after_min".into(),
        json!(data.get("offload_after_min").and_then(py_int).unwrap_or(DEFAULT_OFFLOAD_MIN)),
    );
    m
}

fn save_prefs(updates: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let mut data = load_prefs();
    if let Some(t) = updates.get("theme") {
        let theme = py_str(Some(t), "").to_lowercase();
        if !matches!(theme.as_str(), "light" | "dark" | "system") {
            return Err("theme must be light, dark, or system".into());
        }
        data.insert("theme".into(), json!(theme));
    }
    if updates.contains_key("advanced") {
        data.insert("advanced".into(), json!(truthy(updates.get("advanced"))));
    }
    if let Some(v) = updates.get("offload_after_min") {
        let minutes = py_int(v).ok_or("offload_after_min must be a whole number of minutes")?;
        if !(0..=OFFLOAD_MAX_MIN).contains(&minutes) {
            return Err(format!("offload_after_min must be 0–{OFFLOAD_MAX_MIN}"));
        }
        data.insert("offload_after_min".into(), json!(minutes));
    }
    let dir = &paths().state_dir;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&Value::Object(data.clone())).unwrap_or_default() + "\n";
    fs::write(dir.join("prefs.json"), text).map_err(|e| e.to_string())?;
    Ok(prefs_payload_from(&data))
}

// --- Predict-sleep status (gui/predict_sleep.py, read side) ------------------
fn recent_log(limit: usize) -> Vec<String> {
    let log = PREDICT_LOG.lock().unwrap();
    log.iter().skip(log.len().saturating_sub(limit)).cloned().collect()
}

fn push_log(msg: String) {
    let mut log = PREDICT_LOG.lock().unwrap();
    log.push_back(msg);
    if log.len() > 200 {
        let drop = log.len() - 100;
        log.drain(..drop);
    }
}

fn bandit_counts() -> (i64, i64) {
    let Some(Value::Object(snap)) = read_json(&paths().state_dir.join("linucb.json")) else {
        return (0, 0);
    };
    let arm_ok = |k: &str| {
        snap.get(k).and_then(Value::as_object).is_some_and(|a| a.get("a").is_some_and(Value::is_array) && a.get("b").is_some_and(Value::is_array))
    };
    if snap.get("layout").and_then(Value::as_i64) != Some(FEATURE_LAYOUT)
        || snap.get("dim").and_then(Value::as_i64) != Some(FEATURE_DIM)
        || !arm_ok("keep")
        || !arm_ok("pause")
    {
        return (0, 0);
    }
    let int = |k: &str| snap.get(k).and_then(py_int).unwrap_or(0);
    (int("updates"), int("decisions"))
}

fn predict_status() -> Map<String, Value> {
    let raw = read_json(&paths().state_dir.join("predict-sleep-state.json"));
    let raw = raw.as_ref().and_then(Value::as_object);
    let enabled = raw.map_or(true, |r| if r.contains_key("enabled") { truthy(r.get("enabled")) } else { true });
    let (updates, decisions) = bandit_counts();
    let now = now_f64();
    let mut profiles = Map::new();
    if let Some(Value::Object(ps)) = raw.and_then(|r| r.get("profiles")) {
        for (name, pdata) in ps {
            let Some(pd) = pdata.as_object().filter(|_| profile_ok(name)) else { continue };
            let last_used = pd.get("last_used_at").and_then(py_float).unwrap_or(now);
            let auto = pd.get("auto_paused_at").filter(|v| !v.is_null()).and_then(py_float);
            let pending_action = pd
                .get("pending")
                .and_then(Value::as_object)
                .and_then(|p| p.get("action"))
                .cloned()
                .unwrap_or(Value::Null);
            let wakes = pd
                .get("recent_forced_wakes")
                .and_then(Value::as_array)
                .map_or(0, |a| a.iter().filter_map(Value::as_f64).filter(|t| *t >= now - FORCED_WAKE_WINDOW_S).count());
            let mut m = Map::new();
            m.insert("idle_ms".into(), json!((((now - last_used) * 1000.0) as i64).max(0)));
            m.insert("auto_paused_at".into(), auto.map_or(Value::Null, |f| json!(f)));
            m.insert("pending_action".into(), pending_action);
            m.insert("forced_wakes_10m".into(), json!(wakes));
            profiles.insert(name.clone(), Value::Object(m));
        }
    }
    let mut out = Map::new();
    out.insert("enabled".into(), json!(enabled));
    out.insert("updates".into(), json!(updates));
    out.insert("decisions".into(), json!(decisions));
    out.insert("last_reward".into(), Value::Null); // never persisted; a fresh load has none
    out.insert("model".into(), json!(paths().state_dir.join("linucb.json").to_string_lossy()));
    out.insert("profiles".into(), Value::Object(profiles));
    out.insert("log".into(), json!(recent_log(20)));
    out
}

// --- Usual-time panel (gui/habit_suggest.py, read side) ----------------------
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_year(days: i64) -> i64 {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + if m <= 2 { 1 } else { 0 }
}

fn iso_weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7) + 1 // 1970-01-01 was a Thursday
}

fn week_key(days: i64) -> String {
    let thursday = days - (iso_weekday(days) - 1) + 3;
    let year = civil_year(thursday);
    let week = (thursday - days_from_civil(year, 1, 1)) / 7 + 1;
    format!("{year}-W{week:02}")
}

fn panel_for_now(resumed: &HashSet<String>) -> Value {
    let myt = now_f64() as i64 + MYT_OFFSET_SECS;
    let days = myt.div_euclid(86400);
    let hour = myt.rem_euclid(86400) / 3600;
    let slot = format!("{}-{hour:02}", iso_weekday(days));
    let data = read_json(&paths().state_dir.join("habit.json"));
    let weeks = data.as_ref().and_then(|d| d.get("weeks")).and_then(Value::as_object);
    let mut keys: Vec<String> = vec![];
    for i in 0..WINDOW_WEEKS {
        let k = week_key(days - 7 * i);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut counts: Vec<(String, i64)> = vec![];
    for k in &keys {
        let Some(names) = weeks.and_then(|w| w.get(k)).and_then(Value::as_object).and_then(|w| w.get(&slot)).and_then(Value::as_array) else {
            continue;
        };
        let mut seen: HashSet<&str> = HashSet::new();
        for n in names.iter().filter_map(Value::as_str) {
            if !profile_ok(n) || !seen.insert(n) {
                continue;
            }
            match counts.iter_mut().find(|(c, _)| c == n) {
                Some(e) => e.1 += 1,
                None => counts.push((n.to_string(), 1)),
            }
        }
    }
    let Some(best) = counts.iter().map(|c| c.1).max() else { return Value::Null };
    let winners: Vec<&String> = counts.iter().filter(|c| c.1 == best).map(|c| &c.0).collect();
    if best < MIN_WEEKS || winners.len() != 1 || resumed.contains(winners[0]) {
        return Value::Null;
    }
    json!(winners[0])
}

// --- Chrome update (gui/chrome_update.py) ------------------------------------
static UPDATE_CHECK: Mutex<()> = Mutex::new(());

fn chrome_version_parts(version: &str) -> Vec<u64> {
    let version = version.trim();
    let version = version.split_once(':').map_or(version, |(_, rest)| rest);
    let version = version.split_once('-').map_or(version, |(core, _)| core);
    version.split('.').filter_map(|part| part.parse().ok()).collect()
}

fn installed_chrome_version() -> Option<String> {
    let script = paths().script.to_str()?;
    let out = simple_cmd(&[script, "chrome-image-version"], 52)?;
    if out.code != Some(0) {
        return None;
    }
    let text = out.stdout.trim();
    if text.is_empty() {
        return None;
    }
    let version = text
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .map(|part| part.trim_matches('.'))
        .find(|part| part.contains('.') && part.split('.').all(|n| !n.is_empty()));
    Some(version.unwrap_or(text).into())
}

fn upstream_chrome_version() -> Option<String> {
    let out = simple_cmd(&[
        "bash", "-o", "pipefail", "-c",
        "curl -fsSL --max-time 10 --user-agent 'DockedBrowser/1.0 (Debian)' \
         https://dl.google.com/linux/chrome/deb/dists/stable/main/binary-amd64/Packages.gz | gzip -dc",
    ], 12)?;
    if out.code != Some(0) {
        return None;
    }
    out.stdout
        .split("\n\n")
        .find(|entry| entry.lines().any(|line| line == "Package: google-chrome-stable"))?
        .lines()
        .find_map(|line| line.strip_prefix("Version:"))
        .map(|version| version.trim().to_owned())
}

fn update_check_due(state: &Map<String, Value>, now: f64) -> bool {
    let last = state.get("last_checked_at").and_then(py_float).unwrap_or(0.0);
    let interval = if truthy(state.get("check_failed")) { RETRY_AFTER_FAIL_S } else { CHECK_INTERVAL_S };
    !state.contains_key("update_available") || now - last >= interval
}

fn merge_chrome_probe(
    mut state: Map<String, Value>,
    installed_probe: Option<String>,
    upstream_probe: Option<String>,
) -> Map<String, Value> {
    let failed = installed_probe.is_none() || upstream_probe.is_none();
    let any_probe = installed_probe.is_some() || upstream_probe.is_some();
    let installed = installed_probe.map(Value::String)
        .or_else(|| state.remove("installed_version")).unwrap_or(Value::Null);
    let upstream = upstream_probe.map(Value::String)
        .or_else(|| state.remove("latest_version")).unwrap_or(Value::Null);
    let has_versions = truthy(Some(&installed)) && truthy(Some(&upstream));

    if !failed || !has_versions {
        let notified = state.remove("last_notified_at");
        state.clear();
        if let Some(notified) = notified.filter(|value| truthy(Some(value))) {
            state.insert("last_notified_at".into(), notified);
        }
    }
    if any_probe || !has_versions {
        let installed_parts = chrome_version_parts(installed.as_str().unwrap_or_default());
        let upstream_parts = chrome_version_parts(upstream.as_str().unwrap_or_default());
        let newer = !installed_parts.is_empty() && !upstream_parts.is_empty() && upstream_parts > installed_parts;
        state.insert("update_available".into(), json!(newer));
    }
    if failed {
        state.insert("check_failed".into(), json!(true));
    }
    state.insert("installed_version".into(), installed);
    state.insert("latest_version".into(), upstream);
    state
}

fn chrome_check(force: bool) -> Value {
    let _guard = UPDATE_CHECK.lock().unwrap_or_else(|e| e.into_inner());
    let path = paths().state_dir.join("chrome-update-check.json");
    let state = read_json(&path).and_then(|state| state.as_object().cloned()).unwrap_or_default();
    let now = now_f64();
    if !force && !update_check_due(&state, now) {
        return Value::Object(state);
    }
    let mut state = merge_chrome_probe(state, installed_chrome_version(), upstream_chrome_version());
    state.insert("last_checked_at".into(), json!(now));
    let now_utc = unsafe { libc::time(std::ptr::null_mut()) };
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    if !unsafe { libc::gmtime_r(&now_utc, &mut utc) }.is_null() {
        state.insert("last_checked_iso".into(), json!(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            utc.tm_year + 1900, utc.tm_mon + 1, utc.tm_mday, utc.tm_hour, utc.tm_min, utc.tm_sec
        )));
    }
    let state = Value::Object(state);
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let save = || -> std::io::Result<()> {
        fs::create_dir_all(&paths().state_dir)?;
        fs::write(&temp, serde_json::to_string_pretty(&state)? + "\n")?;
        fs::rename(&temp, &path)
    };
    if let Err(error) = save() {
        let _ = fs::remove_file(&temp);
        eprintln!("Cannot save Chrome update state: {error}");
    }
    state
}

fn update_status_payload() -> Value {
    chrome_check(false)
}

fn status_payload() -> Map<String, Value> {
    let mut containers = docker_chrome_status();
    let profiles = known_profiles(&containers);
    let prefs = load_prefs();
    let theme_pref = prefs.get("theme").and_then(Value::as_str).unwrap_or("system").to_string();
    let theme_resolved = resolve_theme(&theme_pref);
    let predict = predict_status();
    let auto_paused: HashSet<String> = predict
        .get("profiles")
        .and_then(Value::as_object)
        .map(|ps| {
            ps.iter()
                .filter(|(_, info)| truthy(info.get("auto_paused_at")))
                .map(|(n, _)| n.clone())
                .collect()
        })
        .unwrap_or_default();
    for row in &mut containers {
        let ap = auto_paused.contains(row_str(row, "profile")) && row_bool(row, "paused");
        row.insert("auto_paused".into(), json!(ap));
    }
    let resumed: HashSet<String> = containers
        .iter()
        .filter(|r| row_bool(r, "running") && !row_bool(r, "paused"))
        .map(|r| row_str(r, "profile").to_string())
        .collect();
    let icons: Map<String, Value> = profiles.iter().map(|n| (n.clone(), json!(icon_path(n).is_file()))).collect();

    let mut out = Map::new();
    out.insert("image_built".into(), json!(image_built()));
    out.insert("containers".into(), Value::Array(containers.into_iter().map(Value::Object).collect()));
    out.insert("profiles".into(), json!(profiles));
    out.insert("icons".into(), Value::Object(icons));
    out.insert("for_now".into(), panel_for_now(&resumed));
    out.insert("chrome_update".into(), update_status_payload());
    out.insert("theme".into(), json!(theme_resolved));
    out.insert("theme_pref".into(), json!(theme_pref));
    out.insert("theme_resolved".into(), json!(theme_resolved));
    out.insert("advanced".into(), prefs.get("advanced").cloned().unwrap_or(json!(false)));
    out.insert("offload_after_min".into(), prefs.get("offload_after_min").cloned().unwrap_or(json!(DEFAULT_OFFLOAD_MIN)));
    out.insert(
        "predict_sleep".into(),
        json!({
            "enabled": predict.get("enabled").cloned().unwrap_or(json!(true)),
            "updates": predict.get("updates").cloned().unwrap_or(json!(0)),
            "last_reward": predict.get("last_reward").cloned().unwrap_or(Value::Null),
        }),
    );
    out
}

fn parse_pct(raw: &str) -> Value {
    let t = raw.trim().trim_end_matches('%');
    if t.is_empty() || t == "--" {
        return Value::Null;
    }
    t.parse::<f64>().map_or(Value::Null, |f| json!((f * 10.0).round() / 10.0))
}

fn stat_row(r: &Map<String, Value>, cpu: Value, mem_pct: Value, used: &str, limit: &str, usage: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for k in ["name", "profile", "state", "paused", "running"] {
        m.insert(k.into(), r.get(k).cloned().unwrap_or(Value::Null));
    }
    m.insert("cpu_pct".into(), cpu);
    m.insert("mem_pct".into(), mem_pct);
    m.insert("mem_used".into(), json!(used));
    m.insert("mem_limit".into(), json!(limit));
    m.insert("mem_usage".into(), json!(usage));
    m
}

/// One-shot CPU/RAM for chrome-* containers (running + paused).
fn docker_resource_stats() -> Vec<Map<String, Value>> {
    let live: Vec<Map<String, Value>> =
        docker_chrome_status().into_iter().filter(|r| row_bool(r, "running") || row_bool(r, "paused")).collect();
    if live.is_empty() {
        return vec![];
    }
    let mut args = vec!["docker", "stats", "--no-stream", "--format", "{{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}"];
    args.extend(live.iter().map(|r| row_str(r, "name")));
    let Some(out) = simple_cmd(&args, 20) else {
        return live.iter().map(|r| stat_row(r, Value::Null, Value::Null, "", "", "")).collect();
    };
    let mut rows = vec![];
    let mut seen: HashSet<String> = HashSet::new();
    for line in out.stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 || !parts[0].starts_with("chrome-") {
            continue;
        }
        let Some(meta) = live.iter().find(|r| row_str(r, "name") == parts[0]) else { continue };
        let mem = parts[2].trim();
        let (used, limit) = mem.split_once(" / ").map_or((mem, ""), |(u, l)| (u.trim(), l.trim()));
        seen.insert(parts[0].to_string());
        rows.push(stat_row(meta, parse_pct(parts[1]), parse_pct(parts[3]), used, limit, mem));
    }
    for r in &live {
        if !seen.contains(row_str(r, "name")) {
            rows.push(stat_row(r, Value::Null, Value::Null, "", "", ""));
        }
    }
    rows.sort_by(|a, b| {
        (!row_bool(a, "running"), row_str(a, "profile")).cmp(&(!row_bool(b, "running"), row_str(b, "profile")))
    });
    rows
}

// --- Custom dock icons -------------------------------------------------------
fn decode_data_url(data_url: &str) -> Result<Vec<u8>, String> {
    let Some((header, b64)) = data_url.strip_prefix("data:").and_then(|_| data_url.split_once(',')) else {
        return Err("Expected a data: URL image".into());
    };
    if !header.to_lowercase().contains("image/") {
        return Err("Only image uploads are allowed".into());
    }
    // b64decode(validate=False) drops characters outside the alphabet.
    let clean: String = b64.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')).collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new().with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let raw = engine.decode(clean.trim_end_matches('=')).map_err(|e| format!("Invalid base64: {e}"))?;
    if raw.len() > MAX_ICON_BYTES {
        return Err("Image too large (max 2 MB)".into());
    }
    if raw.len() < 32 {
        return Err("Image too small".into());
    }
    let known = raw.starts_with(b"\x89PNG\r\n\x1a\n")
        || raw.starts_with(b"\xff\xd8\xff")
        || (&raw[..4] == b"RIFF" && &raw[8..12] == b"WEBP")
        || raw.starts_with(b"GIF87a")
        || raw.starts_with(b"GIF89a");
    if !known {
        return Err("Unsupported image type (use PNG, JPEG, WebP, or GIF)".into());
    }
    Ok(raw)
}

fn save_icon(profile: &str, data_url: &str) -> Map<String, Value> {
    if !profile_ok(profile) {
        return result(false, "Invalid profile name");
    }
    // Keep uploaded PNG bytes intact and normalize other formats to PNG.
    let png = match decode_data_url(data_url).and_then(|raw| {
        if raw.starts_with(b"\x89PNG\r\n\x1a\n") { Ok(raw) } else { convert_binary(raw) }
    }) {
        Ok(p) => p,
        Err(e) => return result(false, e),
    };
    let dest = icon_path(profile);
    if let Some(dir) = dest.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fs::write(&dest, png) {
        return result(false, e.to_string());
    }
    let dock = run_cockpit("refresh-dock", Some(profile));
    if !truthy(dock.get("ok")) {
        return result(false, format!("Icon saved, but dock refresh failed:\n{}", row_str(&dock, "output")));
    }
    let mut r = result(
        true,
        format!("Custom icon saved for “{profile}”. Re-open the window (or unpin/repin) if the dock still shows the old picture."),
    );
    r.extend(status_payload());
    r
}

/// Decode the first frame and fit it within 256×256 without enlarging it.
fn convert_binary(raw: Vec<u8>) -> Result<Vec<u8>, String> {
    use image::{DynamicImage, ImageFormat, ImageReader, Limits};
    use std::io::Cursor;

    let mut reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .map_err(|e| format!("Cannot read icon: {e}"))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| format!("Cannot decode icon: {e}"))?;
    let image = if image.width() > 256 || image.height() > 256 {
        image.resize(256, 256, image::imageops::FilterType::CatmullRom)
    } else {
        image
    };
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.into_rgba8())
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|e| format!("Cannot encode icon: {e}"))?;
    Ok(png.into_inner())
}

fn clear_icon(profile: &str) -> Map<String, Value> {
    if !profile_ok(profile) {
        return result(false, "Invalid profile name");
    }
    let path = icon_path(profile);
    if path.is_file() {
        let _ = fs::remove_file(&path);
    }
    let dock = run_cockpit("refresh-dock", Some(profile));
    let ok = truthy(dock.get("ok"));
    let mut r = if ok {
        result(true, format!("Cleared custom icon for “{profile}” (back to name badge)."))
    } else {
        result(false, row_str(&dock, "output"))
    };
    r.extend(status_payload());
    r
}

// --- HTTP --------------------------------------------------------------------
struct Request {
    method: String,
    target: String,
    line: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return None;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let line = lines.next()?.to_string();
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY_BYTES {
        return None;
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Some(Request { method, target, line, body })
}

struct Response {
    code: u16,
    body: Vec<u8>,
    content_type: &'static str,
    cache: &'static str,
}

fn resp(code: u16, body: impl Into<Vec<u8>>, content_type: &'static str) -> Response {
    Response { code, body: body.into(), content_type, cache: "no-store" }
}

fn resp_json(code: u16, payload: Map<String, Value>) -> Response {
    resp(code, serde_json::to_vec(&Value::Object(payload)).unwrap_or_default(), "application/json; charset=utf-8")
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        501 => "Unsupported method",
        _ => "",
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn handle_get(path: &str) -> Response {
    let p = paths();
    if path == "/" {
        return match fs::read(&p.template) {
            Ok(html) => resp(200, html, "text/html; charset=utf-8"),
            Err(e) => resp(500, e.to_string(), "text/plain; charset=utf-8"),
        };
    }
    if let Some((_, file, ctype)) = STATIC_FILES.iter().find(|(url, _, _)| *url == path) {
        return match fs::read(p.static_dir.join(file)) {
            Ok(body) => Response { code: 200, body, content_type: ctype, cache: "public, max-age=86400" },
            Err(e) => resp(404, e.to_string(), "text/plain; charset=utf-8"),
        };
    }
    match path {
        "/api/status" => return resp_json(200, status_payload()),
        "/api/chrome-update" => {
            let v = update_status_payload();
            return resp_json(200, v.as_object().cloned().unwrap_or_default());
        }
        "/api/stats" => {
            let rows = docker_resource_stats().into_iter().map(Value::Object).collect();
            let mut m = Map::new();
            m.insert("containers".into(), Value::Array(rows));
            return resp_json(200, m);
        }
        "/api/prefs" => return resp_json(200, prefs_payload_from(&load_prefs())),
        "/api/predict-sleep" => return resp_json(200, predict_status()),
        _ => {}
    }
    if let Some(rest) = path.strip_prefix("/api/icon/") {
        let profile = percent_decode(rest);
        if !profile_ok(&profile) {
            return resp(400, "bad profile", "text/plain");
        }
        return match fs::read(icon_path(&profile)) {
            Ok(png) => resp(200, png, "image/png"),
            Err(_) => resp(404, "no custom icon", "text/plain"),
        };
    }
    resp(404, "Not found", "text/plain; charset=utf-8")
}

fn handle_post(path: &str, body: &[u8]) -> Response {
    let text = String::from_utf8_lossy(body);
    let payload: Value = if text.is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return resp_json(400, result(false, "Invalid JSON")),
        }
    };
    let empty = Map::new();
    let obj = payload.as_object().unwrap_or(&empty);

    match path {
        "/api/prefs" => {
            return match save_prefs(obj) {
                Ok(prefs) => {
                    let mut m = Map::new();
                    m.insert("ok".into(), json!(true));
                    m.extend(prefs);
                    resp_json(200, m)
                }
                Err(e) => resp_json(400, result(false, e)),
            };
        }
        "/api/predict-sleep" => {
            if !payload.is_object() {
                return resp_json(400, result(false, "Invalid JSON"));
            }
            if obj.contains_key("enabled") {
                if let Err(error) = sleep_state::set_enabled(truthy(obj.get("enabled"))) {
                    return resp_json(500, result(false, format!("Cannot update predict-sleep: {error}")));
                }
            }
            let mut m = Map::new();
            m.insert("ok".into(), json!(true));
            m.extend(predict_status());
            return resp_json(200, m);
        }
        "/api/chrome-update" => {
            let mut m = Map::new();
            m.insert("ok".into(), json!(true));
            m.extend(chrome_check(true).as_object().cloned().unwrap_or_default());
            return resp_json(200, m);
        }
        "/api/icon" => {
            let profile = py_str(obj.get("profile"), "").trim().to_string();
            let action = py_str(obj.get("action"), "set").trim().to_lowercase();
            let r = if action == "clear" {
                clear_icon(&profile)
            } else {
                save_icon(&profile, &py_str(obj.get("image"), ""))
            };
            let code = if truthy(r.get("ok")) { 200 } else { 400 };
            return resp_json(code, r);
        }
        "/api/action" => {}
        _ => return resp(404, "Not found", "text/plain; charset=utf-8"),
    }

    let mut action = py_str(obj.get("action"), "").trim().to_lowercase();
    let profile = py_str(obj.get("profile"), "default").trim().to_string();
    let profile = if profile.is_empty() { "default".to_string() } else { profile };
    if !ALLOWED_ACTIONS.contains(&action.as_str()) {
        return resp_json(400, result(false, "Unknown action"));
    }
    // Open / Resume always target the exact profile via activate when possible:
    // paused → unpause, stopped → run, running → focus/active.
    if action == "run" || action == "resume" {
        let rows = docker_chrome_status();
        let row = rows.iter().find(|r| row_str(r, "profile") == profile);
        if action == "resume" || row.is_some_and(|r| row_bool(r, "paused")) {
            action = "activate".into();
        } else if action == "run" && row.is_some_and(|r| row_bool(r, "running")) {
            action = "activate".into();
        }
    }
    let mut r = if action == "build" || FLEET_ACTIONS.contains(&action.as_str()) {
        run_cockpit(&action, None)
    } else {
        run_cockpit(&action, Some(&profile))
    };
    let ok = truthy(r.get("ok"));
    r.extend(status_payload());
    resp_json(if ok { 200 } else { 500 }, r)
}

fn log_stamp() -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!(
        "{:02}/{}/{} {:02}:{:02}:{:02}",
        tm.tm_mday,
        MON[tm.tm_mon.clamp(0, 11) as usize],
        tm.tm_year + 1900,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn handle_conn(mut stream: TcpStream) {
    let Some(req) = read_request(&mut stream) else { return };
    let path = req.target.split(['?', '#']).next().unwrap_or("/").to_string();
    let r = match req.method.as_str() {
        "GET" => handle_get(&path),
        "POST" => handle_post(&path, &req.body),
        _ => resp(501, "Unsupported method", "text/plain; charset=utf-8"),
    };
    println!("[{}] \"{}\" {} -", log_stamp(), req.line, r.code);
    let head = format!(
        "HTTP/1.0 {} {}\r\nServer: DockedBrowser/1.0\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: {}\r\nConnection: close\r\n\r\n",
        r.code,
        reason(r.code),
        r.content_type,
        r.body.len(),
        r.cache
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&r.body);
    let _ = stream.flush();
}

// --- Background helpers --------------------------------------------------------
fn new_session(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Start the focus watcher if none is running (gui/wake_paused.py `ensure_watcher`).
fn ensure_watcher() {
    let p = paths();
    let rust = p.root.join("wake-paused/target/release/wake-paused");
    let exec = fs::metadata(&rust).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    let mut env = desktop_env();
    let mut cmd = if exec {
        let mut c = Command::new(&rust);
        c.arg("--cli").arg(&p.script);
        c
    } else {
        let extra = env.get("PYTHONPATH").cloned().unwrap_or_default();
        let gui = p.gui.to_string_lossy().into_owned();
        env.insert("PYTHONPATH".into(), if extra.is_empty() { gui } else { format!("{gui}:{extra}") });
        let mut c = Command::new("python3");
        c.arg(p.gui.join("wake_paused.py"));
        c
    };
    cmd.env_clear().envs(env).current_dir(&p.gui).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    new_session(&mut cmd);
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap it whenever it exits (it exits at once when the lock is held).
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("wake watcher: {e}"),
    }
}

fn lock_is_free(path: &Path) -> bool {
    let Ok(f) = fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path) else {
        return false;
    };
    use std::os::fd::AsRawFd;
    let fd = f.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return false;
    }
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    true
}

/// server.py ran the predict-sleep loop as a thread. Here it is a child that
/// dies with this process; its `[predict-sleep]` lines feed `/api/predict-sleep`.
fn ensure_daemon() -> bool {
    let p = paths();
    let _ = fs::create_dir_all(&p.state_dir);
    if !lock_is_free(&p.state_dir.join("predict-sleep.lock")) {
        return false;
    }
    let mut cmd = Command::new(p.root.join("bin/docked-predict-sleep"));
    cmd.env_clear()
        .envs(desktop_env())
        .env("PYTHONUNBUFFERED", "1")
        .current_dir(&p.gui)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else { return false };
    let Some(stdout) = child.stdout.take() else { return false };
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("{line}");
            if let Some(msg) = line.strip_prefix("[predict-sleep] ") {
                push_log(msg.to_string());
            }
        }
        let _ = child.wait();
    });
    true
}

fn main() {
    let Some(root) = find_root() else {
        eprintln!("Cannot find the docked_browser checkout (set DOCKED_BROWSER_ROOT).");
        std::process::exit(1);
    };
    let p = PATHS.get_or_init(|| init_paths(root));
    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    if command.as_deref() == Some(std::ffi::OsStr::new("profile-icon")) {
        let args: Vec<_> = args.collect();
        let [name, color, custom, output] = args.as_slice() else {
            eprintln!("Usage: profile-icon NAME COLOR CUSTOM_PATH OUTPUT_PNG");
            std::process::exit(2);
        };
        let result = profile_icon::write(
            name.to_str().unwrap_or_default(),
            color.to_str().unwrap_or_default(),
            Path::new(custom),
            Path::new(output),
        );
        if let Err(error) = result {
            eprintln!("Cannot write profile icon: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("start-wake-watcher")) {
        ensure_watcher();
        return;
    }
    let policy_command = command.as_ref().and_then(|value| value.to_str());
    if matches!(policy_command, Some("predict-policy" | "update-policy")) {
        let mut input = Vec::new();
        let result = std::io::stdin().take(64 * 1024 + 1).read_to_end(&mut input)
            .ok()
            .filter(|&length| length <= 64 * 1024)
            .and_then(|_| serde_json::from_slice(&input).ok())
            .and_then(|input| {
                if policy_command == Some("update-policy") {
                    sleep_state::update_policy(&input)
                } else {
                    sleep_state::select_policy(&input)
                }
            });
        match result {
            Some(result) => println!("{result}"),
            None => {
                eprintln!("Invalid sleep policy input");
                std::process::exit(2);
            }
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("save-sleep-checkpoint")) {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let name = args.next().and_then(|value| value.into_string().ok()).ok_or("Missing checkpoint name")?;
            let mut input = Vec::new();
            std::io::stdin().take(4 * 1024 * 1024 + 1).read_to_end(&mut input)?;
            if input.len() > 4 * 1024 * 1024 {
                return Err("Checkpoint input exceeds 4 MiB".into());
            }
            sleep_state::save_checkpoint(&name, &serde_json::from_slice(&input)?)?;
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("Cannot save sleep checkpoint: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("predict-daemon")) {
        if let Err(error) = scheduler::run() {
            eprintln!("Predict-sleep daemon: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("mark-profile-used")) {
        let profile = args.next().and_then(|value| value.into_string().ok()).unwrap_or_default();
        if let Err(error) = sleep_state::mark_used(&profile) {
            eprintln!("Cannot record profile use: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("log-profile-use")) {
        let profile = args.next().and_then(|value| value.into_string().ok()).unwrap_or_default();
        match habit::log_use(&profile) {
            Ok(added) => println!("{added}"),
            Err(error) => {
                eprintln!("Cannot record habit: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("offload-tick")) {
        if let Err(error) = offload::tick() {
            eprintln!("offload state error: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("seed-chrome-fonts")) {
        let Some(path) = args.next() else {
            eprintln!("Usage: seed-chrome-fonts PREFERENCES_PATH");
            std::process::exit(2);
        };
        if let Err(error) = seed_chrome_fonts(Path::new(&path)) {
            eprintln!("Cannot seed Chrome fonts: {error}");
            std::process::exit(1);
        }
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("status")) {
        println!("{:#}", cli_status());
        return;
    }
    if command.as_deref() == Some(std::ffi::OsStr::new("check-update")) {
        let state = chrome_check(true);
        println!("{}", serde_json::to_string_pretty(&state).expect("serializable update state"));
        if truthy(state.get("update_available")) {
            let latest = state["latest_version"].as_str().unwrap_or("unknown");
            let installed = state["installed_version"].as_str().unwrap_or("unknown");
            println!("Update available: {latest} (installed: {installed})");
            println!("Run `./bin/docked-browser build` or click Install Update in the app to install.");
        } else {
            let installed = state["installed_version"].as_str().unwrap_or("latest");
            println!("Google Chrome is up to date ({installed}).");
        }
        return;
    }
    if let Some(command) = command {
        eprintln!("Unknown command: {}", command.to_string_lossy());
        std::process::exit(2);
    }
    if !p.template.is_file() {
        eprintln!("Missing template: {}", p.template.display());
        std::process::exit(1);
    }
    let host = std::env::var("DOCKED_BROWSER_HOST")
        .or_else(|_| std::env::var("COCKPIT_HOST"))
        .unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("DOCKED_BROWSER_PORT")
        .or_else(|_| std::env::var("COCKPIT_PORT"))
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8787);

    if std::env::var_os("DOCKED_BROWSER_NO_DAEMON").is_some() {
        println!("Predict-sleep daemon: skipped (DOCKED_BROWSER_NO_DAEMON)");
    } else if ensure_daemon() {
        println!("Predict-sleep daemon: running (LinUCB per-profile auto-pause)");
        ensure_watcher();
    } else {
        println!("Predict-sleep daemon: already held by another process");
        ensure_watcher();
    }

    let listener = match TcpListener::bind((host.as_str(), port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {host}:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("Docked Browser GUI → http://{host}:{port}");
    println!("Press Ctrl+C to stop.");
    let _ = std::io::stdout().flush();
    for stream in listener.incoming().flatten() {
        // One thread per connection, like ThreadingHTTPServer.
        let _ = thread::Builder::new().stack_size(256 * 1024).spawn(move || handle_conn(stream));
    }
}

#[cfg(test)]
mod icon_tests {
    use super::convert_binary;
    use image::{DynamicImage, ImageFormat, RgbImage};
    use std::io::Cursor;

    #[test]
    fn converts_browser_icon_formats_to_rgba_png() {
        for format in [ImageFormat::Jpeg, ImageFormat::WebP, ImageFormat::Gif] {
            for (width, height, expected) in [(512, 256, (256, 128)), (48, 24, (48, 24))] {
                let image = DynamicImage::ImageRgb8(RgbImage::new(width, height));
                let mut input = Cursor::new(Vec::new());
                image.write_to(&mut input, format).unwrap();

                let png = convert_binary(input.into_inner()).unwrap();
                let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png).unwrap();
                assert_eq!((decoded.width(), decoded.height()), expected, "{format:?}");
                assert!(decoded.as_rgba8().is_some(), "{format:?}");
            }
        }
    }

    #[test]
    fn rejects_truncated_image() {
        assert!(convert_binary(b"GIF89a".to_vec()).is_err());
    }
}

#[cfg(test)]
mod chrome_update_tests {
    use super::{merge_chrome_probe, update_check_due};
    use serde_json::json;

    #[test]
    fn failed_probe_preserves_known_update_and_retries_early() {
        let state = json!({
            "installed_version": "154.0.1.2",
            "latest_version": "155.0.1.2-1",
            "update_available": true,
            "last_notified_at": 20,
        });
        let mut merged = merge_chrome_probe(state.as_object().unwrap().clone(), None, None);
        assert_eq!(merged["update_available"], true);
        assert_eq!(merged["check_failed"], true);
        assert_eq!(merged["last_notified_at"], 20);
        merged.insert("last_checked_at".into(), json!(100));
        assert!(!update_check_due(&merged, 3699.0));
        assert!(update_check_due(&merged, 3700.0));

        let fresh = merge_chrome_probe(merged, Some("155.0.1.2".into()), Some("1:155.0.1.2-1".into()));
        assert_eq!(fresh["update_available"], false);
        assert!(!fresh.contains_key("check_failed"));
        assert_eq!(fresh["last_notified_at"], 20);
    }
}

#[cfg(test)]
mod predict_sleep_route_tests {
    use super::*;

    #[test]
    fn toggle_keeps_existing_profile_state() {
        let state_dir = std::env::temp_dir().join(format!(
            "docked-sleep-route-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir(&state_dir).unwrap();
        let mut p = init_paths(PathBuf::new());
        p.state_dir = state_dir.clone();
        assert!(PATHS.set(p).is_ok());
        let path = state_dir.join("predict-sleep-state.json");
        let profiles = json!({"Work": {"last_used_at": 123, "last_wall_input_at": 123}});
        fs::write(&path, json!({"enabled": true, "profiles": profiles}).to_string()).unwrap();

        let response = handle_post("/api/predict-sleep", br#"{"enabled":false}"#);
        let saved = read_json(&path).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_file(state_dir.join("predict-sleep-state.lock")).unwrap();
        fs::remove_dir(state_dir).unwrap();
        let payload: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["enabled"], false);
        assert_eq!(saved, json!({"enabled": false, "profiles": profiles}));
    }
}
