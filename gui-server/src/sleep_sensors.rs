use serde_json::{Value, json};

use super::{active_profile, profile_ok, py_int, simple_cmd};

fn output(args: &[&str], timeout: u64) -> Option<String> {
    let result = simple_cmd(args, timeout)?;
    (result.code == Some(0)).then(|| result.stdout.trim().to_owned())
}

fn first_number(text: &str) -> Option<u64> {
    text.split(|ch: char| !ch.is_ascii_digit())
        .find(|part| !part.is_empty())?
        .parse()
        .ok()
}

fn idle_ms() -> u64 {
    output(
        &[
            "gdbus",
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.IdleMonitor",
            "--object-path",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "--method",
            "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ],
        3,
    )
    .and_then(|reply| {
        // The numeric type name inside a GVariant reply is not part of the value.
        first_number(
            reply
                .trim_start_matches(['(', ' '])
                .trim_start_matches("uint64"),
        )
    })
    .or_else(|| output(&["xprintidle"], 3)?.parse().ok())
    .unwrap_or(0)
}

fn shell_focus(reply: &str) -> Option<(i64, String)> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    let data: Value = serde_json::from_str(reply.get(start..=end)?).ok()?;
    let class = ["wm_class", "wm_class_instance"]
        .into_iter()
        .filter_map(|key| data.get(key)?.as_str())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if data["focus"] == false {
        return Some((0, class));
    }
    let pid = data.get("pid").and_then(py_int).filter(|&pid| pid > 0)?;
    Some((pid, class))
}

fn focused_window() -> Option<(i64, String)> {
    let shell = output(
        &[
            "gdbus",
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell",
            "--object-path",
            "/org/gnome/shell/extensions/FocusedWindow",
            "--method",
            "org.gnome.shell.extensions.FocusedWindow.Get",
        ],
        2,
    );
    if let Some(focus) = shell.as_deref().and_then(shell_focus) {
        return Some(focus);
    }
    let window = output(&["xdotool", "getactivewindow"], 3)?;
    window.parse::<u64>().ok()?;
    let class = output(&["xprop", "-id", &window, "WM_CLASS"], 3).unwrap_or_default();
    let pid = first_number(&output(&["xprop", "-id", &window, "_NET_WM_PID"], 3)?)?;
    Some((i64::try_from(pid).ok()?, class.to_ascii_lowercase()))
}

pub(super) fn snapshot() -> Value {
    let running = output(
        &[
            "docker",
            "ps",
            "--filter",
            "name=chrome-",
            "--filter",
            "status=running",
            "--format",
            "{{.Names}}",
        ],
        15,
    )
    .unwrap_or_default();
    let profiles: Vec<_> = running
        .lines()
        .map(str::trim)
        .filter_map(|name| name.strip_prefix("chrome-").map(|profile| (profile, name)))
        .filter(|(profile, _)| profile_ok(profile))
        .collect();
    if profiles.is_empty() {
        return json!({"profiles": [], "focused": "", "active": "", "idle_ms": 0});
    }

    let idle = idle_ms();
    let focused = focused_window()
        .and_then(|(pid, class)| {
            if pid <= 0
                || (!class.is_empty()
                    && !["docked-browser", "chrome", "chromium"]
                        .iter()
                        .any(|name| class.contains(name)))
            {
                return None;
            }
            profiles.iter().find_map(|&(profile, name)| {
                let processes = output(&["docker", "top", name, "-eo", "pid"], 8)?;
                processes
                    .lines()
                    .any(|line| line.trim().parse::<i64>() == Ok(pid))
                    .then_some(profile)
            })
        })
        .unwrap_or_default();
    let names: Vec<_> = profiles.iter().map(|(profile, _)| *profile).collect();
    json!({"profiles": names, "focused": focused, "active": active_profile(), "idle_ms": idle})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_focus_prefers_reported_class_and_handles_unfocused_windows() {
        assert_eq!(
            shell_focus(
                "('{\"pid\":72,\"wm_class\":\"Chrome\",\"wm_class_instance\":\"other\"}',)"
            ),
            Some((72, "chrome".into()))
        );
        assert_eq!(
            shell_focus("('{\"pid\":72,\"focus\":false}',)"),
            Some((0, String::new()))
        );
        assert_eq!(shell_focus("('{\"pid\":\"unknown\"}',)"), None);
    }
}
