//! Resume a paused profile when its window is focused (GNOME Wayland).
//!
//! Rust port of `gui/wake_paused.py`. Same behaviour, but it talks to the
//! Focused Window helper over D-Bus and to dockerd over its Unix socket
//! directly, instead of spawning `gdbus` and `docker ps` on every poll.
//!
//! Left-click on a GNOME Dash icon for a mapped Chrome window raises it by
//! `StartupWMClass` without re-running Exec, so across `docker pause` a Dash
//! click only raises the frozen window. This watcher polls the helper: when
//! the focused class is `docked-browser-<profile>` and that container is
//! paused, it runs `bin/docked-browser activate <profile>`.
//!
//! Focus that lands within `WORKSPACE_SWITCH` of a bare workspace switch
//! (keyboard, swipe, overview thumbnail) does not wake. The helper reports
//! `bare_switch_age_ms` once its patch is loaded at login; until then every
//! focus enter wakes.
//!
//! `--dry-run` logs "would wake" instead of running `activate`, and uses its
//! own lock, so it can run next to the live watcher.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(350);
const WAKE_GAP: Duration = Duration::from_secs(2);
/// Focus that lands this soon after a bare workspace switch came from the switch.
const WORKSPACE_SWITCH: f64 = 1.0;
const PAUSED_CACHE: Duration = Duration::from_secs(1);
const DOCKER_SOCK: &str = "/var/run/docker.sock";

fn valid_profile(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Profile name inside a WM_CLASS value (`docked-browser-<name>`, any case), or "".
fn profile_from_wm_class(wm: &str) -> String {
    let lower = wm.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("docked-browser-") {
        let start = from + i + "docked-browser-".len();
        let name: String = wm[start..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(64)
            .collect();
        if valid_profile(&name) {
            return name;
        }
        from = start;
    }
    String::new()
}

struct Logger {
    path: PathBuf,
}

impl Logger {
    fn log(&self, line: &str) {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let stamp = local_hms(secs as i64);
        let msg = format!("[{stamp}] {line}\n");
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut fh) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = fh.write_all(msg.as_bytes());
        }
        print!("[wake-paused] {msg}");
        let _ = std::io::stdout().flush();
    }
}

/// HH:MM:SS in local time, via the libc that std already links.
fn local_hms(secs: i64) -> String {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        _rest: [i32; 6],
        _gmtoff: i64,
        _zone: *const u8,
    }
    unsafe extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm = Tm { sec: 0, min: 0, hour: 0, _rest: [0; 6], _gmtoff: 0, _zone: std::ptr::null() };
    // SAFETY: localtime_r only writes into `tm`, which is a valid struct tm.
    if unsafe { localtime_r(&secs, &mut tm) }.is_null() {
        return "??:??:??".into();
    }
    format!("{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec)
}

/// What one poll of the focus source reports.
#[derive(Debug, Clone, Default, PartialEq)]
struct Focus {
    profile: String,
    /// Seconds since the last bare workspace switch; None before the patch or any switch.
    switch_age: Option<f64>,
}

/// The outside world `tick` needs; swapped for a fake in tests.
trait World {
    fn focus(&mut self) -> Focus;
    fn paused(&mut self, profile: &str) -> bool;
    fn wake(&mut self, profile: &str);
    fn log(&mut self, line: &str);
}

/// What the previous poll saw.
#[derive(Default)]
struct FocusState {
    profile: String,
    suppressed: HashSet<String>,
}

/// One poll. Wake on focus *enter* into a paused profile.
///
/// Not woken: a profile paused while focused (until focus leaves it), and one
/// that got focus because of a bare workspace switch (not a window pick).
fn tick(state: &mut FocusState, world: &mut impl World) {
    let Focus { profile: cur, switch_age } = world.focus();
    let prev = std::mem::take(&mut state.profile);
    if !prev.is_empty() && prev != cur {
        state.suppressed.remove(&prev);
    }
    if !cur.is_empty() && world.paused(&cur) {
        if cur != prev {
            if switch_age.is_some_and(|age| age <= WORKSPACE_SWITCH) {
                world.log(&format!("not waking {cur}: focused by workspace switch"));
                state.suppressed.insert(cur.clone());
            } else if !state.suppressed.contains(&cur) {
                world.wake(&cur);
            }
        } else {
            // Still focused when it became / stayed paused — wait until focus leaves.
            state.suppressed.insert(cur.clone());
        }
    }
    state.profile = cur;
}

struct Live {
    dbus: Option<zbus::blocking::Connection>,
    paused_cache: Option<(Instant, HashSet<String>)>,
    last_wake: HashMap<String, Instant>,
    cli: PathBuf,
    dry_run: bool,
    logger: Logger,
}

impl Live {
    /// Raw JSON from FocusedWindow.Get, reconnecting to the session bus if needed.
    fn helper_get(&mut self) -> Option<String> {
        if self.dbus.is_none() {
            self.dbus = zbus::blocking::Connection::session().ok();
        }
        let conn = self.dbus.as_ref()?;
        let reply = conn.call_method(
            Some("org.gnome.Shell"),
            "/org/gnome/shell/extensions/FocusedWindow",
            Some("org.gnome.shell.extensions.FocusedWindow"),
            "Get",
            &(),
        );
        match reply {
            Ok(msg) => msg.body().deserialize::<String>().ok(),
            Err(zbus::Error::InputOutput(_)) => {
                self.dbus = None;
                None
            }
            Err(_) => None,
        }
    }

    fn focus_from_shell(&mut self) -> (String, Option<f64>) {
        let Some(raw) = self.helper_get() else { return (String::new(), None) };
        let Ok(serde_json::Value::Object(info)) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return (String::new(), None);
        };
        // An empty object (old helper, nothing focused) counts as no answer.
        if info.is_empty() {
            return (String::new(), None);
        }
        let age = info.get("bare_switch_age_ms").and_then(|v| v.as_f64()).map(|ms| ms / 1000.0);
        if info.get("focus") == Some(&serde_json::Value::Bool(false)) {
            return (String::new(), age);
        }
        let text = |k: &str| match info.get(k) {
            Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        (text("wm_class").or_else(|| text("wm_class_instance")).unwrap_or_default(), age)
    }

    /// Leftover X11 focus probe — not a supported product path.
    fn focus_x11() -> String {
        let run = |cmd: &str, args: &[&str]| {
            Command::new(cmd)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        let wid = run("xdotool", &["getactivewindow"]);
        if wid.is_empty() || !wid.bytes().all(|b| b.is_ascii_digit()) {
            return String::new();
        }
        run("xprop", &["-id", &wid, "WM_CLASS"])
    }

    /// Profiles whose `chrome-*` container is paused, from dockerd's API.
    fn query_paused() -> Option<HashSet<String>> {
        let mut sock = UnixStream::connect(DOCKER_SOCK).ok()?;
        sock.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        sock.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
        // filters={"name":["chrome-"],"status":["paused"]}
        let req = "GET /containers/json?all=1&filters=%7B%22name%22%3A%5B%22chrome-%22%5D%2C%22status%22%3A%5B%22paused%22%5D%7D HTTP/1.0\r\nHost: docker\r\n\r\n";
        sock.write_all(req.as_bytes()).ok()?;
        let mut resp = Vec::new();
        sock.read_to_end(&mut resp).ok()?;
        let split = resp.windows(4).position(|w| w == b"\r\n\r\n")?;
        if !resp.starts_with(b"HTTP/1.") || !resp[..split].windows(5).any(|w| w == b" 200 ") {
            return None;
        }
        let rows: serde_json::Value = serde_json::from_slice(&resp[split + 4..]).ok()?;
        let mut out = HashSet::new();
        for row in rows.as_array()? {
            let names = row.get("Names").and_then(|n| n.as_array());
            for name in names.into_iter().flatten().filter_map(|n| n.as_str()) {
                if let Some(p) = name.trim_start_matches('/').strip_prefix("chrome-") {
                    if valid_profile(p) {
                        out.insert(p.to_string());
                    }
                }
            }
        }
        Some(out)
    }
}

impl World for Live {
    fn focus(&mut self) -> Focus {
        let (mut wm, switch_age) = self.focus_from_shell();
        if wm.is_empty() && switch_age.is_none() {
            wm = Self::focus_x11();
        }
        Focus { profile: profile_from_wm_class(&wm), switch_age }
    }

    fn paused(&mut self, profile: &str) -> bool {
        if !valid_profile(profile) {
            return false;
        }
        let fresh = self.paused_cache.as_ref().is_some_and(|(at, _)| at.elapsed() < PAUSED_CACHE);
        if !fresh {
            // Like the Python version, a failed query caches "nothing paused".
            self.paused_cache = Some((Instant::now(), Self::query_paused().unwrap_or_default()));
        }
        self.paused_cache.as_ref().is_some_and(|(_, set)| set.contains(profile))
    }

    fn wake(&mut self, profile: &str) {
        if self.last_wake.get(profile).is_some_and(|t| t.elapsed() < WAKE_GAP) {
            return;
        }
        self.last_wake.insert(profile.to_string(), Instant::now());
        if self.dry_run {
            self.logger.log(&format!("would wake {profile} (dry run)"));
            return;
        }
        if !self.cli.is_file() {
            self.logger.log(&format!("missing {}", self.cli.display()));
            return;
        }
        self.logger.log(&format!("waking {profile}"));
        // Bust the paused cache so we do not re-fire before docker unpause lands.
        self.paused_cache = None;
        let mut cmd = Command::new(&self.cli);
        cmd.arg("activate")
            .arg(profile)
            .current_dir(self.cli.parent().unwrap_or(Path::new("/")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        apply_desktop_env(&mut cmd);
        if let Ok(mut child) = cmd.spawn() {
            // Reap in the background so no zombie is left behind.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }

    fn log(&mut self, line: &str) {
        self.logger.log(line);
    }
}

/// Wayland half of `control.desktop_env()` (the X11 branch is an unsupported leftover).
fn apply_desktop_env(cmd: &mut Command) {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("/run/user/{}", unsafe_getuid()));
    cmd.env("XDG_RUNTIME_DIR", &runtime);
    if std::env::var_os("DISPLAY").is_none() {
        cmd.env("DISPLAY", ":0");
    }
    let has_wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty());
    if !has_wayland && Path::new(&runtime).join("wayland-0").exists() {
        cmd.env("WAYLAND_DISPLAY", "wayland-0");
    }
}

fn unsafe_getuid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { getuid() }
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("docked-browser")
}

/// `bin/docked-browser`, found from `--cli`, else next to the repo this binary was built in.
fn find_cli(arg: Option<String>) -> PathBuf {
    if let Some(p) = arg {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    for dir in exe.ancestors().skip(1) {
        let cand = dir.join("bin/docked-browser");
        if cand.is_file() {
            return cand;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bin/docked-browser")
}

fn acquire_lock(path: &Path) -> Option<File> {
    fs::create_dir_all(path.parent()?).ok()?;
    let mut file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path).ok()?;
    file.try_lock().ok()?;
    file.set_len(0).ok()?;
    writeln!(file, "{}", std::process::id()).ok()?;
    Some(file)
}

fn main() {
    let mut dry_run = false;
    let mut probe = false;
    let mut cli = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--probe" => probe = true,
            "--cli" => cli = args.next(),
            "-h" | "--help" => {
                println!("usage: wake-paused [--dry-run | --probe] [--cli PATH/TO/bin/docked-browser]");
                return;
            }
            other => {
                eprintln!("wake-paused: unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if probe {
        // One live reading, for checking the D-Bus and Docker paths by hand.
        let mut live = Live {
            dbus: None,
            paused_cache: None,
            last_wake: HashMap::new(),
            cli: find_cli(cli),
            dry_run: true,
            logger: Logger { path: PathBuf::from("/dev/null") },
        };
        let mut paused: Vec<_> = Live::query_paused().unwrap_or_default().into_iter().collect();
        paused.sort();
        println!("focus: {:?}\npaused: {paused:?}\ncli: {}", live.focus(), live.cli.display());
        return;
    }
    let dir = state_dir();
    let suffix = if dry_run { "-dry-run" } else { "" };
    let Some(_lock) = acquire_lock(&dir.join(format!("wake-paused{suffix}.lock"))) else {
        return; // another watcher already runs
    };
    let mut world = Live {
        dbus: None,
        paused_cache: None,
        last_wake: HashMap::new(),
        cli: find_cli(cli),
        dry_run,
        logger: Logger { path: dir.join(format!("wake-paused{suffix}.log")) },
    };
    world.logger.log("watching focus (GNOME Focused Window / Dash raise)");
    let mut state = FocusState::default();
    loop {
        tick(&mut state, &mut world);
        sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    //! Same cases as gui/test_wake_paused.py. No Docker, no D-Bus.
    use super::*;

    struct Fake {
        focus: Focus,
        paused: HashSet<String>,
        woken: Vec<String>,
    }

    impl World for Fake {
        fn focus(&mut self) -> Focus {
            self.focus.clone()
        }
        fn paused(&mut self, p: &str) -> bool {
            self.paused.contains(p)
        }
        fn wake(&mut self, p: &str) {
            self.woken.push(p.to_string());
        }
        fn log(&mut self, _: &str) {}
    }

    struct Case {
        world: Fake,
        state: FocusState,
    }

    impl Case {
        fn new() -> Self {
            let paused = ["Relax", "Stoc"].iter().map(|s| s.to_string()).collect();
            Case { world: Fake { focus: Focus::default(), paused, woken: vec![] }, state: FocusState::default() }
        }
        fn poll(&mut self, profile: &str, switch_age: Option<f64>) {
            self.world.focus = Focus { profile: profile.into(), switch_age };
            tick(&mut self.state, &mut self.world);
        }
    }

    #[test]
    fn focus_without_any_switch_wakes() {
        let mut c = Case::new();
        c.poll("", None);
        c.poll("Relax", None);
        assert_eq!(c.world.woken, ["Relax"]);
    }

    #[test]
    fn dock_click_onto_other_workspace_wakes() {
        let mut c = Case::new();
        c.poll("", Some(120.0));
        c.poll("Relax", Some(120.4));
        assert_eq!(c.world.woken, ["Relax"]);
    }

    #[test]
    fn bare_switch_focus_does_not_wake() {
        let mut c = Case::new();
        c.poll("", Some(60.0));
        c.poll("Relax", Some(0.2));
        c.poll("Stoc", Some(0.3));
        assert!(c.world.woken.is_empty());
    }

    #[test]
    fn focus_one_poll_after_bare_switch_does_not_wake() {
        let mut c = Case::new();
        c.poll("", Some(0.1));
        c.poll("Relax", Some(WORKSPACE_SWITCH / 2.0));
        assert!(c.world.woken.is_empty());
    }

    #[test]
    fn click_after_switch_settles_wakes() {
        let mut c = Case::new();
        c.poll("", Some(30.0));
        c.poll("Relax", Some(0.2));
        c.poll("", Some(3.0));
        c.poll("Relax", Some(4.0));
        assert_eq!(c.world.woken, ["Relax"]);
    }

    #[test]
    fn paused_while_focused_waits_for_focus_to_leave() {
        let mut c = Case::new();
        c.poll("Relax", None);
        c.world.woken.clear();
        c.poll("Relax", None);
        c.poll("Relax", None);
        assert!(c.world.woken.is_empty());
    }

    #[test]
    fn wm_class_parsing() {
        assert_eq!(profile_from_wm_class("docked-browser-Relax"), "Relax");
        assert_eq!(profile_from_wm_class("WM_CLASS(STRING) = \"x\", \"Docked-Browser-Ham\""), "Ham");
        assert_eq!(profile_from_wm_class("org.gnome.Ptyxis"), "");
        assert_eq!(profile_from_wm_class("docked-browser-"), "");
    }
}
