use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

use super::{
    ensure_watcher, notifications, offload, paths, sleep_poll, sleep_sensors, sleep_state,
};

const POLL_INTERVAL: Duration = Duration::from_secs(60);

pub(super) fn run() -> io::Result<()> {
    let p = paths();
    fs::create_dir_all(&p.state_dir)?;
    let mut lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(p.state_dir.join("predict-sleep.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        println!("predict-sleep already running elsewhere");
        ensure_watcher();
        return Ok(());
    }
    lock.set_len(0)?;
    writeln!(lock, "{}", std::process::id())?;

    ensure_watcher();
    let checkpoint = p.state_dir.join("linucb.json");
    if !checkpoint.is_file() {
        sleep_state::mutate(|_| {
            if !checkpoint.is_file() {
                let model = sleep_state::Model::load(&checkpoint);
                model.save_tick(&checkpoint, model.decisions)?;
            }
            Ok(())
        })?;
    }
    println!(
        "Docked Browser predict-sleep (poll={}s)",
        POLL_INTERVAL.as_secs()
    );
    loop {
        thread::sleep(POLL_INTERVAL);
        let snapshot = sleep_sensors::snapshot();
        notifications::background(&snapshot);
        if let Err(error) = sleep_poll::tick(&snapshot) {
            sleep_poll::log(format!("tick error: {error}"));
        }
        if let Err(error) = offload::tick() {
            sleep_poll::log(format!("offload error: {error}"));
        }
    }
}
