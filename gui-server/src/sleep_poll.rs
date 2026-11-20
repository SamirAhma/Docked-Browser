use serde_json::{Map, Value, json};
use std::{io, process::Command, time::Duration};

use super::{
    FORCED_WAKE_WINDOW_S, MYT_OFFSET_SECS, habit, notifications, now_f64, paths, py_float,
    read_json, run_capture, sleep_sensors, sleep_state, truthy,
};

const MIN_IDLE: i64 = 60_000;
const BREAKER: i64 = 300_000;

pub(super) fn log(message: impl std::fmt::Display) {
    println!("[predict-sleep] [{}] {message}", super::log_stamp());
}

fn reward(pause: bool, idle: i64, wall: i64) -> f64 {
    if pause {
        if idle.max(wall) >= BREAKER {
            0.5
        } else if idle < 15_000 {
            -1.0
        } else {
            0.0
        }
    } else if idle < 30_000 {
        1.0
    } else if idle >= BREAKER {
        -0.5
    } else {
        0.2 - (idle - 30_000) as f64 / (BREAKER - 30_000) as f64 * 0.7
    }
}

fn number(value: &Value, key: &str, default: f64) -> f64 {
    value
        .get(key)
        .and_then(py_float)
        .filter(|n| n.is_finite())
        .unwrap_or(default)
}

fn pause(profile: &str) -> bool {
    let mut cmd = Command::new(&paths().script);
    cmd.args(["pause", profile]);
    run_capture(cmd, Duration::from_secs(60), None).is_ok_and(|output| output.code == Some(0))
}

pub(super) fn tick(snapshot: &Value) -> io::Result<()> {
    let dir = &paths().state_dir;
    let state = read_json(&dir.join("predict-sleep-state.json")).unwrap_or(Value::Null);
    if state
        .get("enabled")
        .is_some_and(|enabled| !truthy(Some(enabled)))
    {
        return Ok(());
    }
    let running = snapshot["profiles"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if running.is_empty() {
        return Ok(());
    }

    let now = now_f64();
    let focus = snapshot["focused"].as_str().unwrap_or_default();
    let idle_at_host = snapshot["idle_ms"].as_i64().unwrap_or_default();
    let model_path = dir.join("linucb.json");
    let mut model = sleep_state::Model::load(&model_path);
    let baseline_decisions = model.decisions;
    let mut changes = Map::new();
    for profile in running.iter().filter_map(Value::as_str) {
        let original = &state["profiles"][profile];
        let mut item = original.as_object().cloned().unwrap_or_default();
        let used = number(original, "last_used_at", now);
        item.entry("last_used_at").or_insert(json!(used));
        item.entry("last_wall_input_at").or_insert(json!(used));
        item.entry("auto_paused_at").or_insert(Value::Null);
        item.entry("cancel_grace_until").or_insert(Value::Null);
        let is_focused = focus == profile;
        let is_active = snapshot["active"] == profile;
        let idle = if is_focused {
            if idle_at_host < MIN_IDLE {
                let used = json!(now - idle_at_host as f64 / 1000.0);
                item.insert("last_used_at".into(), used.clone());
                item.insert("last_wall_input_at".into(), used);
            }
            idle_at_host
        } else {
            ((now - used).max(0.0) * 1000.0) as i64
        };
        let mut wakes = original["recent_forced_wakes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        wakes.retain(|entry| py_float(entry).is_some_and(|at| at >= now - FORCED_WAKE_WINDOW_S));
        if let Some(pending) = original["pending"].as_object() {
            let pending = Value::Object(pending.clone());
            let was_pause = pending["action"]
                .as_str()
                .is_some_and(|action| action != "KeepRunning");
            let wall = ((now - number(&pending, "decided_at", now)) * 1000.0) as i64;
            model.observe(
                profile,
                &pending,
                now,
                was_pause,
                reward(was_pause, idle, wall),
            );
            if was_pause && idle < 15_000 && wall < BREAKER {
                wakes.push(json!(now));
            }
        }
        let angle =
            (now + MYT_OFFSET_SECS as f64).rem_euclid(86400.0) * std::f64::consts::TAU / 86400.0;
        let mut context = json!({
            "decided_at": now, "idle_ms": idle, "is_active": is_active,
            "chrome_focused": is_focused, "cpu_busy": false,
            "hour_sin": angle.sin(), "hour_cos": angle.cos(),
            "recent_forced_wakes": wakes.len(),
        });
        wakes.drain(..wakes.len().saturating_sub(32));
        item.insert("recent_forced_wakes".into(), json!(wakes));
        let breaker = idle >= BREAKER && !is_focused;
        let decision = if breaker {
            Value::Null
        } else {
            model.select(profile, &context, now).unwrap_or(Value::Null)
        };
        let action = if breaker {
            "Pause"
        } else {
            decision["action"].as_str().unwrap_or("KeepRunning")
        };
        context["action"] = json!(action);
        let mut pending = !breaker;
        if action == "Pause" && !is_focused && idle >= MIN_IDLE {
            let reason = if breaker {
                format!("BREAKER idle_ms={idle}")
            } else {
                let mode = if decision["explored"] == true {
                    "explore"
                } else {
                    "action"
                };
                let focused = if focus.is_empty() { "-" } else { focus };
                format!(
                    "{mode}=Pause idle_ms={idle} active={} focused={focused} n={}",
                    u8::from(is_active),
                    model.updates
                )
            };
            let until = number(original, "cancel_grace_until", 0.0);
            if until > now_f64() {
                log(format!(
                    "{profile}: skip pause (cancel grace {}s left) reason={reason}",
                    (until - now_f64()) as i64
                ));
                pending = false;
            } else {
                log(format!("{profile}: warning (20s) reason={reason}"));
                let mut probe = sleep_sensors::FocusProbe::new(profile);
                let cancelled = notifications::pause_cancelled(profile, || probe.focused());
                // A manual open during the countdown also cancels a pending pause.
                let latest =
                    read_json(&dir.join("predict-sleep-state.json")).unwrap_or(Value::Null);
                let newest = &latest["profiles"][profile];
                let was_opened = number(newest, "last_wall_input_at", 0.0)
                    > number(original, "last_wall_input_at", 0.0);
                let enabled = latest
                    .get("enabled")
                    .is_none_or(|value| truthy(Some(value)));
                if cancelled || was_opened {
                    let when = now_f64();
                    item.insert("last_used_at".into(), json!(when));
                    item.insert("last_wall_input_at".into(), json!(when));
                    item.insert("auto_paused_at".into(), Value::Null);
                    item.insert("cancel_grace_until".into(), json!(when + 900.0));
                    let _ = habit::log_use(profile);
                    model.observe(profile, &context, now, true, -1.0);
                    log(format!(
                        "{profile}: warning cancelled → Pause −1.0 (grace 15m)"
                    ));
                    pending = false;
                } else if enabled && pause(profile) {
                    item.insert("auto_paused_at".into(), json!(now_f64()));
                    item.insert("cancel_grace_until".into(), Value::Null);
                    if breaker {
                        model.observe(profile, &context, now, true, 0.5);
                    }
                    log(format!("{profile}: {reason} → Pause (after warning)"));
                } else {
                    log(format!("{profile}: {reason} pause skipped or failed"));
                    pending = false;
                }
            }
        } else if action == "KeepRunning" {
            let keep = decision["scores"][0].as_f64().unwrap_or_default();
            let pause = decision["scores"][1].as_f64().unwrap_or_default();
            log(format!(
                "{profile}: KeepRunning idle_ms={idle} ucb=({keep:.3},{pause:.3}) n={}",
                model.updates
            ));
        }
        item.insert(
            "pending".into(),
            if pending { context } else { Value::Null },
        );
        changes.insert(profile.into(), Value::Object(item));
    }

    sleep_state::mutate(|current| {
        let profiles = current["profiles"]
            .as_object_mut()
            .ok_or_else(|| io::Error::other("invalid sleep profiles"))?;
        for (profile, replacement) in changes {
            let latest = profiles.get(&profile).unwrap_or(&Value::Null);
            if number(latest, "last_wall_input_at", 0.0)
                <= number(&replacement, "last_wall_input_at", 0.0)
            {
                profiles.insert(profile, replacement);
            }
        }
        model.save_tick(&model_path, baseline_decisions)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_thresholds_match_the_legacy_policy() {
        assert_eq!(reward(true, 14_999, 299_999), -1.0);
        assert_eq!(reward(true, 14_999, 300_000), 0.5);
        assert_eq!(reward(true, 15_000, 299_999), 0.0);
        assert_eq!(reward(false, 29_999, 0), 1.0);
        assert_eq!(reward(false, 30_000, 0), 0.2);
        assert_eq!(reward(false, 300_000, 0), -0.5);
    }
}
