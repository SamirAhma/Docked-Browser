#!/usr/bin/env python3
"""Resume a paused profile when its window is clicked.

Cinnamon sends ``_NET_WM_PING`` on every button press. ``docker pause``
freezes Chrome, so the ping gets no reply and the shell shows
"Docked · <name> is not responding" after 5 seconds. Resuming on that
click lets Chrome answer before the timeout.
"""

from __future__ import annotations

import fcntl
import os
import re
import subprocess
import sys
import time
from pathlib import Path

from Xlib import X, display
from Xlib.error import XError
from Xlib.ext import xinput

from control import desktop_env, session_wants_x11

GUI_DIR = Path(__file__).resolve().parent
CLI = GUI_DIR.parent / "bin" / "docked-browser"
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
LOCK_PATH = STATE_DIR / "wake-paused.lock"
LOG_PATH = STATE_DIR / "wake-paused.log"

PROFILE_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")
CLASS_RE = re.compile(r"^docked-browser-([A-Za-z0-9_-]{1,64})$")
WAKE_GAP_S = 2.0

_last_wake: dict[str, float] = {}


def profile_from_wm_class(wm: object) -> str:
    """Return the profile name stored in a WM_CLASS value, or ''."""
    if not wm:
        return ""
    parts = wm if isinstance(wm, (list, tuple)) else (str(wm),)
    for part in parts:
        match = CLASS_RE.match(str(part or "").strip())
        if match and PROFILE_RE.match(match.group(1)):
            return match.group(1)
    return ""


def _log(line: str) -> None:
    stamp = time.strftime("%H:%M:%S")
    msg = f"[{stamp}] {line}\n"
    try:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        with LOG_PATH.open("a", encoding="utf-8") as fh:
            fh.write(msg)
    except OSError:
        pass
    print(f"[wake-paused] {msg}", end="", flush=True)


def _profile_of_window(win: object, root_id: int) -> str:
    current = win
    seen: set[int] = set()
    for _ in range(64):
        if current is None:
            return ""
        wid = int(getattr(current, "id", 0) or 0)
        if wid == 0 or wid in seen or wid == root_id:
            return ""
        seen.add(wid)
        try:
            profile = profile_from_wm_class(current.get_wm_class())
        except XError:
            profile = ""
        if profile:
            return profile
        try:
            current = current.query_tree().parent
        except XError:
            return ""
    return ""


def profile_at_pointer(disp: display.Display) -> str:
    """Profile whose top-level window is under the pointer, or ''."""
    root = disp.screen().root
    win = root
    for _ in range(32):
        try:
            pointed = win.query_pointer()
        except XError:
            return ""
        child = pointed.child
        if child is None or int(getattr(child, "id", 0) or 0) in (0, X.NONE, win.id):
            break
        win = child
    return _profile_of_window(win, int(root.id))


def container_paused(profile: str) -> bool:
    if not PROFILE_RE.match(profile):
        return False
    try:
        result = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Status}}", f"chrome-{profile}"],
            capture_output=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0 and result.stdout.strip() == "paused"


def request_wake(profile: str) -> None:
    now = time.monotonic()
    if now - _last_wake.get(profile, 0.0) < WAKE_GAP_S:
        return
    _last_wake[profile] = now
    if not CLI.is_file():
        _log(f"missing {CLI}")
        return
    _log(f"waking {profile}")
    subprocess.Popen(
        [str(CLI), "activate", profile],
        env=desktop_env(),
        cwd=str(CLI.parent),
        start_new_session=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _on_button(disp: display.Display, event: object) -> None:
    if getattr(event, "type", None) != 35:
        return
    if getattr(event, "evtype", None) != xinput.RawButtonPress:
        return
    profile = profile_at_pointer(disp)
    if not profile or not container_paused(profile):
        return
    request_wake(profile)


def watch() -> None:
    disp = display.Display()
    if not disp.has_extension("XInputExtension"):
        raise RuntimeError("XInput extension missing")
    disp.xinput_query_version()
    root = disp.screen().root
    root.xinput_select_events([(xinput.AllMasterDevices, xinput.RawButtonPressMask)])
    disp.flush()
    _log("watching clicks")
    while True:
        event = disp.next_event()
        try:
            _on_button(disp, event)
        except Exception as exc:  # noqa: BLE001 — one click must not stop the watcher
            _log(f"click handler: {exc}")


def _acquire_lock() -> int | None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    fd = os.open(str(LOCK_PATH), os.O_CREAT | os.O_RDWR, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(fd)
        return None
    os.ftruncate(fd, 0)
    os.write(fd, f"{os.getpid()}\n".encode())
    return fd


def main() -> int:
    if not session_wants_x11():
        _log(
            "skip: not an X11 session "
            f"(XDG_SESSION_TYPE={os.environ.get('XDG_SESSION_TYPE', '')})"
        )
        return 0
    lock_fd = _acquire_lock()
    if lock_fd is None:
        return 0
    while True:
        try:
            watch()
        except Exception as exc:  # noqa: BLE001 — reconnect after an X drop
            _log(f"restart: {exc}")
            time.sleep(2)
    return 0


def ensure_watcher() -> None:
    """Start the click watcher if this session is X11 and none is running."""
    if not session_wants_x11():
        return
    env = desktop_env()
    extra = env.get("PYTHONPATH", "")
    env["PYTHONPATH"] = str(GUI_DIR) + (os.pathsep + extra if extra else "")
    subprocess.Popen(
        [sys.executable, str(Path(__file__).resolve())],
        env=env,
        cwd=str(GUI_DIR),
        start_new_session=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


if __name__ == "__main__":
    raise SystemExit(main())
