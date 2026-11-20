use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::{fd::AsRawFd, unix::process::CommandExt};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use super::{desktop_env, ensure_watcher, paths};

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const COMPLETE: &str = "docked-predict-sleep: tick complete";

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

    let mut worker = Command::new(p.root.join("bin/docked-predict-sleep"));
    worker
        .arg("--poll-worker")
        .env_clear()
        .envs(desktop_env())
        .env("PYTHONUNBUFFERED", "1")
        .env("DOCKER_CONTEXT", "default")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let parent = std::process::id() as libc::pid_t;
    // Reap the worker with the scheduler, including a shutdown during a poll.
    unsafe {
        worker.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "scheduler exited",
                ));
            }
            Ok(())
        });
    }
    let mut child = worker.spawn()?;
    let mut commands = child.stdin.take().expect("piped worker stdin");
    let mut messages = BufReader::new(child.stdout.take().expect("piped worker stdout"));
    ensure_watcher();
    println!(
        "Docked Browser predict-sleep (poll={}s)",
        POLL_INTERVAL.as_secs()
    );
    let result = poll(
        &mut commands,
        &mut messages,
        POLL_INTERVAL,
        || match child.try_wait()? {
            Some(status) => Err(io::Error::other(format!("poll worker exited: {status}"))),
            None => Ok(()),
        },
    );
    drop(commands);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn poll(
    commands: &mut impl Write,
    messages: &mut impl BufRead,
    interval: Duration,
    mut check_worker: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let mut message = String::new();
    loop {
        let start = std::time::Instant::now();
        while let Some(remaining) = interval.checked_sub(start.elapsed()) {
            check_worker()?;
            thread::sleep(remaining.min(Duration::from_secs(1)));
        }
        writeln!(commands, "tick")?;
        commands.flush()?;
        loop {
            message.clear();
            if messages.read_line(&mut message)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "poll worker stopped",
                ));
            }
            if message.trim_end_matches(['\r', '\n']) == COMPLETE {
                break;
            }
            print!("{message}");
            io::stdout().flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_waits_for_completion_and_stops_on_worker_eof() {
        let mut commands = Vec::new();
        let mut messages = io::Cursor::new(format!("{COMPLETE}\n"));
        let result = poll(&mut commands, &mut messages, Duration::ZERO, || Ok(()));
        assert_eq!(commands, b"tick\ntick\n");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
