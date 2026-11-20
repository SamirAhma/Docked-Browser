use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::process::Command;
use std::time::Duration;

use super::{
    DEFAULT_OFFLOAD_MIN, RunError, now_f64, paths, profile_ok, py_int, read_json, run_capture,
    simple_cmd,
};

pub(super) fn tick() -> io::Result<()> {
    let p = paths();
    let minutes = read_json(&p.state_dir.join("prefs.json"))
        .and_then(|prefs| prefs.get("offload_after_min").and_then(py_int))
        .unwrap_or(DEFAULT_OFFLOAD_MIN);
    if minutes <= 0 {
        return Ok(());
    }
    let Some(result) = simple_cmd(
        &[
            "docker",
            "ps",
            "--filter",
            "name=chrome-",
            "--filter",
            "status=paused",
            "--format",
            "{{.Names}}",
        ],
        15,
    )
    .filter(|result| result.code == Some(0)) else {
        return Ok(());
    };
    let paused: BTreeSet<_> = result
        .stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("chrome-"))
        .filter(|name| profile_ok(name))
        .collect();

    let now = now_f64();
    let dir = p.state_dir.join("paused-at");
    fs::create_dir_all(&dir)?;
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !paused.contains(name.strip_suffix(".offloaded").unwrap_or(name)) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    for profile in paused {
        let stamp = dir.join(profile);
        let since = fs::read_to_string(&stamp)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite());
        let Some(since) = since else {
            fs::write(stamp, format!("{}\n", now as i64))?;
            continue;
        };
        if now - since < minutes as f64 * 60.0 {
            continue;
        }
        let marker = dir.join(format!("{profile}.offloaded"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(marker)
        {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }

        let mut command = Command::new(&p.script);
        command.args(["offload", profile]);
        let output = match run_capture(command, Duration::from_secs(300), None) {
            Ok(result) => result
                .stdout
                .trim()
                .lines()
                .last()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("offload exit {}", result.code.unwrap_or(-1))),
            Err(RunError::Timeout) => "offload timed out".into(),
            Err(RunError::Os(error)) => format!("offload failed: {error}"),
        };
        println!(
            "[predict-sleep] {profile}: paused {}m → {output}",
            ((now - since) / 60.0).floor() as i64
        );
    }
    Ok(())
}
