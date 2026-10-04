#!/usr/bin/env python3
"""Resume a paused profile when its window is focused (GNOME Wayland).

Left-click on a GNOME Dash icon for a mapped Chrome window activates that
window by ``StartupWMClass`` and does **not** re-run the ``.desktop`` Exec.
Across ``docker pause`` the window stays mapped, so a Dash click only raises
the frozen window. This watcher polls the Focused Window helper: when the
focused class is ``docked-browser-<profile>`` and that container is paused,
it runs ``bin/docked-browser activate <profile>``.

Started from the GUI server, tray, predict-sleep daemon, and on pause.
"""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

from control import desktop_env

GUI_DIR = Path(__file__).resolve().parent
CLI = GUI_DIR.parent / "bin" / "docked-browser"
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
LOCK_PATH = STATE_DIR / "wake-paused.lock"
LOG_PATH = STATE_DIR / "wake-paused.log"

PROFILE_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")
CLASS_RE = re.compile(r"docked-browser-([A-Za-z0-9_-]{1,64})", re.I)

POLL_S = 0.35
WAKE_GAP_S = 2.0
PAUSED_CACHE_S = 1.0

_last_wake: dict[str, float] = {}
_paused_cache: tuple[float, frozenset[str]] = (0.0, frozenset())


def profile_from_wm_class(wm: object) -> str:
    """Return the profile name stored in a WM_CLASS value, or ''."""
    if not wm:
        return ""
    parts = wm if isinstance(wm, (list, tuple)) else (str(wm),)
    for part in parts:
        match = CLASS_RE.search(str(part or "").strip())
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


def _run(cmd: list[str], timeout: float = 2.0) -> str:
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return (result.stdout or "").strip() if result.returncode == 0 else ""


def _parse_focused_payload(out: str) -> dict | None:
    start = out.find("{")
    end = out.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        data = json.loads(out[start : end + 1])
    except json.JSONDecodeError:
        return None
    return data if isinstance(data, dict) else None


def _focused_wm_class_from_shell() -> str:
    """Focused WM class via the Focused Window D-Bus helper (GNOME / Wayland)."""
    out = _run(
        [
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
        timeout=2.0,
    )
    info = _parse_focused_payload(out) if out else None
    if not info or info.get("focus") is False:
        return ""
    return str(info.get("wm_class") or info.get("wm_class_instance") or "")


def _focused_wm_class_x11() -> str:
    """Leftover X11 focus probe — not a supported product path."""
    wid = _run(["xdotool", "getactivewindow"])
    if not wid.isdigit():
        return ""
    return _run(["xprop", "-id", wid, "WM_CLASS"])


def focused_docked_profile() -> str:
    """Profile for the focused docked-browser window, or ''."""
    wm = _focused_wm_class_from_shell()
    if not wm:
        wm = _focused_wm_class_x11()
    return profile_from_wm_class(wm)


def paused_profiles() -> frozenset[str]:
    """Cached set of profile names whose chrome-* container is paused."""
    global _paused_cache
    now = time.monotonic()
    cached_at, cached = _paused_cache
    if now - cached_at < PAUSED_CACHE_S:
        return cached
    out = _run(
        [
            "docker",
            "ps",
            "-a",
            "--filter",
            "name=chrome-",
            "--filter",
            "status=paused",
            "--format",
            "{{.Names}}",
        ],
        timeout=5.0,
    )
    names: set[str] = set()
    for line in out.splitlines():
        name = line.strip()
        if name.startswith("chrome-"):
            profile = name[len("chrome-") :]
            if PROFILE_RE.match(profile):
                names.add(profile)
    frozen = frozenset(names)
    _paused_cache = (now, frozen)
    return frozen


def container_paused(profile: str) -> bool:
    if not PROFILE_RE.match(profile):
        return False
    return profile in paused_profiles()


def request_wake(profile: str) -> None:
    now = time.monotonic()
    if now - _last_wake.get(profile, 0.0) < WAKE_GAP_S:
        return
    _last_wake[profile] = now
    if not CLI.is_file():
        _log(f"missing {CLI}")
        return
    _log(f"waking {profile}")
    # Bust paused cache so we do not re-fire before docker unpause lands.
    global _paused_cache
    _paused_cache = (0.0, frozenset())
    subprocess.Popen(
        [str(CLI), "activate", profile],
        env=desktop_env(),
        cwd=str(CLI.parent),
        start_new_session=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def tick(prev: str, suppressed: set[str]) -> str:
    """One poll. Wake on focus *enter* into a paused profile; suppress pause-in-place."""
    cur = focused_docked_profile()
    if prev and prev != cur:
        suppressed.discard(prev)
    if cur and container_paused(cur):
        if cur != prev:
            if cur not in suppressed:
                request_wake(cur)
        else:
            # Still focused when it became / stayed paused — wait until focus leaves.
            suppressed.add(cur)
    return cur


def watch() -> None:
    _log("watching focus (GNOME Focused Window / Dash raise)")
    prev = ""
    suppressed: set[str] = set()
    while True:
        try:
            prev = tick(prev, suppressed)
        except Exception as exc:  # noqa: BLE001 — one poll must not stop the watcher
            _log(f"tick: {exc}")
        time.sleep(POLL_S)


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
    lock_fd = _acquire_lock()
    if lock_fd is None:
        return 0
    while True:
        try:
            watch()
        except Exception as exc:  # noqa: BLE001 — reconnect after a D-Bus / session drop
            _log(f"restart: {exc}")
            time.sleep(2)
    return 0


def ensure_watcher() -> None:
    """Start the focus watcher if none is running."""
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
