use serde_json::{Map, Value, json};
use std::{fs, io, path::Path};

use super::{
    FEATURE_DIM, FEATURE_LAYOUT, FORCED_WAKE_WINDOW_S, MYT_OFFSET_SECS, now_f64, paths, profile_ok,
    py_float, read_json, truthy,
};

const DIM: usize = FEATURE_DIM as usize;
const ALPHA: f64 = 0.5;
type Features = [f64; DIM];

fn state_object(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn persist(path: &Path, data: &Value) -> io::Result<()> {
    let temp = path.with_extension(format!("json.rust.{}.tmp", std::process::id()));
    let save = || {
        fs::write(&temp, serde_json::to_string_pretty(data)? + "\n")?;
        fs::rename(&temp, path)
    };
    let result = save();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(super) fn save_checkpoint(name: &str, payload: &Value) -> io::Result<()> {
    let valid = match name {
        "linucb.json" => {
            payload["layout"] == FEATURE_LAYOUT
                && payload["dim"] == FEATURE_DIM
                && Arm::from_json(&payload["keep"]).is_some()
                && Arm::from_json(&payload["pause"]).is_some()
        }
        // The Python fallback merges the state first and holds the state lock
        // while calling this writer.
        "predict-sleep-state.json" => {
            payload["enabled"].is_boolean() && payload["profiles"].is_object()
        }
        _ => false,
    };
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid predict-sleep checkpoint",
        ));
    }
    let dir = &paths().state_dir;
    fs::create_dir_all(dir)?;
    persist(&dir.join(name), payload)
}

pub(super) fn mutate(
    edit: impl FnOnce(&mut Map<String, Value>) -> io::Result<()>,
) -> io::Result<()> {
    let dir = &paths().state_dir;
    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("predict-sleep-state.lock"))?;
    lock.lock()?;
    let path = dir.join("predict-sleep-state.json");
    let mut data = state_object(read_json(&path).as_ref());
    data.entry("enabled").or_insert(json!(true));
    data.entry("profiles").or_insert(json!({}));
    edit(&mut data)?;
    persist(&path, &Value::Object(data))
}

pub(super) fn set_enabled(enabled: bool) -> io::Result<()> {
    mutate(|data| {
        data.insert("enabled".into(), json!(enabled));
        Ok(())
    })
}

fn hash_profile(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100000001b3)
    })
}

fn features(pending: &Value, profile: &str, now: f64) -> Features {
    let number = |key, fallback| pending.get(key).and_then(py_float).unwrap_or(fallback);
    let active = u8::from(truthy(pending.get("is_active"))) as f64;
    let focused = u8::from(truthy(pending.get("chrome_focused"))) as f64;
    let idle = (number("idle_ms", 0.0) / 600_000.0).clamp(0.0, 1.0);
    let angle =
        (now + MYT_OFFSET_SECS as f64).rem_euclid(86400.0) * std::f64::consts::TAU / 86400.0;
    let mut x = [0.0; DIM];
    x[0] = 1.0;
    x[1] = active;
    x[2] = focused;
    x[3] = active * focused;
    x[4] = u8::from(truthy(pending.get("cpu_busy"))) as f64;
    x[6] = number("hour_sin", angle.sin());
    x[7] = number("hour_cos", angle.cos());
    x[8] = (number("recent_forced_wakes", 0.0) / 5.0).min(1.0);
    x[9] = idle;
    x[10] = 1.0 - active;
    x[11] = active * idle;
    let hash = hash_profile(&profile.to_ascii_lowercase());
    x[14 + (hash % 2) as usize] = if hash & (1 << 32) == 0 { 1.0 } else { -1.0 };
    x
}

fn vector(value: &Value, count: usize) -> Option<Vec<f64>> {
    let values = value.as_array()?;
    if values.len() != count {
        return None;
    }
    values.iter().map(Value::as_f64).collect()
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn matrix_vector(matrix: &[f64], vector: &[f64]) -> Features {
    std::array::from_fn(|index| dot(&matrix[index * DIM..(index + 1) * DIM], vector))
}

pub(super) fn update_policy(input: &Value) -> Option<Value> {
    let profile = input["profile"].as_str().filter(|name| profile_ok(name))?;
    let context = input.get("context")?;
    let now = input["now"].as_f64()?;
    let reward = input["reward"].as_f64()?.clamp(-1.0, 1.0);
    let x = features(context, profile, now);
    let mut arm = Arm::from_json(&input["arm"])?;
    let mut inv = vector(&input["arm"]["a_inv"], DIM * DIM)?;
    let ax = matrix_vector(&inv, &x);
    let denominator = 1.0 + dot(&x, &ax);
    // The caller can use its existing full matrix inversion for a degenerate input.
    if !denominator.is_finite() || denominator.abs() < 1e-12 {
        return None;
    }
    arm.observe(&x, reward);
    for (i, row) in inv.chunks_exact_mut(DIM).enumerate() {
        for (j, element) in row.iter_mut().enumerate() {
            *element -= ax[i] * ax[j] / denominator;
        }
    }
    if !inv
        .iter()
        .chain(&arm.a)
        .chain(&arm.b)
        .all(|value| value.is_finite())
    {
        return None;
    }
    Some(json!({"a": arm.a, "b": arm.b, "a_inv": inv, "reward": reward}))
}

/// Evaluate the current policy. The daemon supplies its live inverse matrices,
/// which include rewards credited earlier in this poll.
pub(super) fn select_policy(input: &Value) -> Option<Value> {
    let profile = input["profile"].as_str().filter(|name| profile_ok(name))?;
    let context = input.get("context")?;
    let now = input["now"].as_f64()?;
    let alpha = input["alpha"].as_f64()?;
    let x = features(context, profile, now);
    let score = |arm: &Value| -> Option<f64> {
        let inv = vector(&arm["a_inv"], DIM * DIM)?;
        let b = vector(&arm["b"], DIM)?;
        let theta = matrix_vector(&inv, &b);
        let ax = matrix_vector(&inv, &x);
        let score = dot(&theta, &x) + alpha * dot(&x, &ax).max(0.0).sqrt();
        score.is_finite().then_some(score)
    };
    let keep = score(&input["keep"])?;
    let pause = score(&input["pause"])?;
    let focused = truthy(context.get("chrome_focused"));
    let idle = context["idle_ms"].as_i64()?;
    let prefer_pause = if (keep - pause).abs() < 1e-9 {
        !focused && idle >= 60_000
    } else {
        pause > keep
    };
    let explore = !prefer_pause
        && !focused
        && idle >= 90_000
        && hash_profile(&format!("{profile}:{}", (now / 60.0).floor() as i64)) % 1000 < 120;
    Some(json!({
        "action": if prefer_pause || explore { "Pause" } else { "KeepRunning" },
        "explored": explore,
        "scores": [keep, pause],
    }))
}

struct Arm {
    a: Vec<f64>,
    b: Vec<f64>,
}

impl Arm {
    fn identity() -> Self {
        let mut a = vec![0.0; DIM * DIM];
        for i in 0..DIM {
            a[i * DIM + i] = 1.0;
        }
        Self {
            a,
            b: vec![0.0; DIM],
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            a: vector(&value["a"], DIM * DIM)?,
            b: vector(&value["b"], DIM)?,
        })
    }

    fn observe(&mut self, x: &Features, reward: f64) {
        for (i, xi) in x.iter().enumerate() {
            for (j, xj) in x.iter().enumerate() {
                self.a[i * DIM + j] += xi * xj;
            }
            self.b[i] += reward * xi;
        }
    }

    fn json(&self) -> Value {
        json!({"a": self.a, "b": self.b})
    }
}

fn default_model() -> (Arm, Arm) {
    let mut keep = Arm::identity();
    let mut pause = Arm::identity();
    let mut x = [0.0; DIM];
    x[..4].fill(1.0);
    keep.observe(&x, 1.0);
    x = [0.0; DIM];
    x[0] = 1.0;
    x[8] = 1.0;
    keep.observe(&x, 1.0);
    x[8] = 0.0;
    x[9] = 0.6;
    x[10] = 1.0;
    pause.observe(&x, 0.6);
    (keep, pause)
}

fn invert(a: &[f64]) -> Option<Vec<f64>> {
    let mut augmented = [[0.0; DIM * 2]; DIM];
    for i in 0..DIM {
        augmented[i][..DIM].copy_from_slice(&a[i * DIM..(i + 1) * DIM]);
        augmented[i][DIM + i] = 1.0;
    }
    for column in 0..DIM {
        let pivot = (column..DIM).max_by(|&a, &b| {
            augmented[a][column]
                .abs()
                .total_cmp(&augmented[b][column].abs())
        })?;
        if augmented[pivot][column].abs() < 1e-12 {
            return None;
        }
        augmented.swap(column, pivot);
        let divisor = augmented[column][column];
        for x in &mut augmented[column] {
            *x /= divisor;
        }
        let row = augmented[column];
        for (index, target) in augmented.iter_mut().enumerate() {
            if index == column {
                continue;
            }
            let factor = target[column];
            for (value, pivot) in target.iter_mut().zip(row) {
                *value -= factor * pivot;
            }
        }
    }
    Some(
        augmented
            .iter()
            .flat_map(|row| row[DIM..].iter().copied())
            .collect(),
    )
}

pub(super) struct Model {
    arms: [Arm; 2],
    inverse: [Vec<f64>; 2],
    alpha: f64,
    pub decisions: u64,
    pub updates: u64,
    observations: Vec<(usize, Features, f64)>,
}

impl Model {
    pub(super) fn load(path: &Path) -> Self {
        let raw = read_json(path).unwrap_or(Value::Null);
        let valid = (|| {
            if raw["layout"].as_i64()? != FEATURE_LAYOUT || raw["dim"].as_i64()? != FEATURE_DIM {
                return None;
            }
            Some((
                Arm::from_json(&raw["keep"])?,
                Arm::from_json(&raw["pause"])?,
            ))
        })();
        let (keep, pause) = valid.unwrap_or_else(default_model);
        let arms = [keep, pause];
        let inverse =
            std::array::from_fn(|i| invert(&arms[i].a).unwrap_or_else(|| Arm::identity().a));
        Self {
            arms,
            inverse,
            alpha: raw["alpha"].as_f64().unwrap_or(ALPHA),
            decisions: raw["decisions"].as_u64().unwrap_or_default(),
            updates: raw["updates"].as_u64().unwrap_or_default(),
            observations: Vec::new(),
        }
    }

    pub(super) fn observe(
        &mut self,
        profile: &str,
        context: &Value,
        now: f64,
        pause: bool,
        reward: f64,
    ) {
        let index = usize::from(pause);
        let x = features(context, profile, now);
        self.arms[index].observe(&x, reward);
        let inv = &mut self.inverse[index];
        let ax = matrix_vector(inv, &x);
        let denom = 1.0 + dot(&x, &ax);
        if denom.abs() >= 1e-12 && denom.is_finite() {
            for (i, row) in inv.chunks_exact_mut(DIM).enumerate() {
                for (j, value) in row.iter_mut().enumerate() {
                    *value -= ax[i] * ax[j] / denom;
                }
            }
        } else {
            *inv = invert(&self.arms[index].a).unwrap_or_else(|| Arm::identity().a);
        }
        self.updates += 1;
        self.observations.push((index, x, reward));
    }

    pub(super) fn select(&mut self, profile: &str, context: &Value, now: f64) -> Option<Value> {
        self.decisions += 1;
        select_policy(&json!({
            "profile": profile, "context": context, "now": now, "alpha": self.alpha,
            "keep": {"a_inv": self.inverse[0], "b": self.arms[0].b},
            "pause": {"a_inv": self.inverse[1], "b": self.arms[1].b},
        }))
    }

    pub(super) fn save_tick(&self, path: &Path, decisions: u64) -> io::Result<()> {
        // Replay this tick's observations into the latest checkpoint while the
        // profile state lock is held, preserving concurrent manual wake credit.
        let mut current = Self::load(path);
        for (arm, x, reward) in &self.observations {
            current.arms[*arm].observe(x, *reward);
            current.updates += 1;
        }
        current.decisions += self.decisions.saturating_sub(decisions);
        persist(
            path,
            &json!({
                "layout": FEATURE_LAYOUT, "dim": FEATURE_DIM, "alpha": current.alpha,
                "decisions": current.decisions, "updates": current.updates,
                "keep": current.arms[0].json(), "pause": current.arms[1].json(),
            }),
        )
    }
}

fn credit_forced_wake(path: &Path, pending: &Value, profile: &str, now: f64) -> io::Result<()> {
    let existing = read_json(path).unwrap_or(Value::Null);
    let arms = (|| {
        if existing["layout"].as_i64()? != FEATURE_LAYOUT
            || existing["dim"].as_i64()? != FEATURE_DIM
        {
            return None;
        }
        Some((
            Arm::from_json(&existing["keep"])?,
            Arm::from_json(&existing["pause"])?,
        ))
    })();
    let (keep, mut pause, metadata) = match arms {
        Some((keep, pause)) => (keep, pause, existing),
        None => {
            let (keep, pause) = default_model();
            (keep, pause, Value::Null)
        }
    };
    pause.observe(&features(pending, profile, now), -1.0);
    persist(
        path,
        &json!({
            "layout": FEATURE_LAYOUT,
            "dim": FEATURE_DIM,
            "alpha": metadata["alpha"].as_f64().unwrap_or(ALPHA),
            "decisions": metadata["decisions"].as_u64().unwrap_or(0),
            "updates": metadata["updates"].as_u64().unwrap_or(0).saturating_add(1),
            "keep": keep.json(),
            "pause": pause.json(),
        }),
    )
}

pub(super) fn mark_used(profile: &str) -> io::Result<()> {
    if !profile_ok(profile) {
        return Ok(());
    }
    let now = now_f64();
    mutate(|data| {
        let profiles = data.get_mut("profiles").unwrap();
        if !profiles.is_object() {
            *profiles = json!({});
        }
        let profiles = profiles.as_object_mut().unwrap();
        let mut state = state_object(profiles.get(profile));
        let mut wakes = state
            .get("recent_forced_wakes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if state
            .get("auto_paused_at")
            .and_then(py_float)
            .is_some_and(|last| now - last < 300.0)
        {
            wakes.push(json!(now));
            if let Some(pending) = state
                .get("pending")
                .filter(|value| value["action"] == "Pause")
            {
                if let Err(error) = credit_forced_wake(
                    &paths().state_dir.join("linucb.json"),
                    pending,
                    profile,
                    now,
                ) {
                    eprintln!("Cannot record forced wake: {error}");
                }
                state.insert("pending".into(), Value::Null);
            }
        }
        wakes.retain(|wake| py_float(wake).is_some_and(|at| at >= now - FORCED_WAKE_WINDOW_S));
        wakes.drain(..wakes.len().saturating_sub(32));
        state.insert("recent_forced_wakes".into(), json!(wakes));
        state.insert("auto_paused_at".into(), Value::Null);
        state.insert("last_used_at".into(), json!(now));
        state.insert("last_wall_input_at".into(), json!(now));
        state.entry("pending").or_insert(Value::Null);
        state.entry("cancel_grace_until").or_insert(Value::Null);
        profiles.insert(profile.to_owned(), Value::Object(state));
        Ok(())
    })?;
    if let Err(error) = super::habit::log_use(profile) {
        eprintln!("Cannot record habit: {error}");
    }
    Ok(())
}
