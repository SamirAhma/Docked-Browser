#!/usr/bin/env python3
"""Resume a paused profile when its window is focused (GNOME Wayland).

Left-click on a GNOME Dash icon for a mapped Chrome window activates that
window by ``StartupWMClass`` and does **not** re-run the ``.desktop`` Exec.
Across ``docker pause`` the window stays mapped, so a Dash click only raises
the frozen window. This watcher polls the Focused Window helper: when the
focused class is ``docked-browser-<profile>`` and that container is paused,
it runs ``bin/docked-browser activate <profile>``.

Switching workspaces by keyboard (Ctrl+Super+arrow), swipe, or overview
thumbnail also focuses that workspace's top window. The helper reports
``bare_switch_age_ms`` for those switches only; focus that lands within
WORKSPACE_SWITCH_S of one does not wake. A Dash/dock click or Alt+Tab onto a
window on another workspace is not a bare switch, so it still wakes. Before
the patched helper loads (next login) the field is missing and every focus
enter wakes.

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
RUST_WATCHER = GUI_DIR.parent / "wake-paused" / "target" / "release" / "wake-paused"
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
LOCK_PATH = STATE_DIR / "wake-paused.lock"
LOG_PATH = STATE_DIR / "wake-paused.log"

PROFILE_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")
CLASS_RE = re.compile(r"docked-browser-([A-Za-z0-9_-]{1,64})", re.I)

POLL_S = 0.35
WAKE_GAP_S = 2.0
# Focus that lands this soon after a bare workspace switch came from the switch.
WORKSPACE_SWITCH_S = 1.0
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


def _focused_from_shell() -> tuple[str, float | None]:
    """(focused WM class, seconds since the last bare workspace switch).

    The age is None until the bare-switch patch is loaded, or before any switch.
    """
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
    if not info:
        return "", None
    age_ms = info.get("bare_switch_age_ms")
    age = age_ms / 1000.0 if isinstance(age_ms, (int, float)) and not isinstance(age_ms, bool) else None
    if info.get("focus") is False:
        return "", age
    return str(info.get("wm_class") or info.get("wm_class_instance") or ""), age


def _focused_wm_class_x11() -> str:
    """Leftover X11 focus probe — not a supported product path."""
    wid = _run(["xdotool", "getactivewindow"])
    if not wid.isdigit():
        return ""
    return _run(["xprop", "-id", wid, "WM_CLASS"])


def focused_docked_profile() -> tuple[str, float | None]:
    """(focused docked-browser profile or '', seconds since last bare switch)."""
    wm, age = _focused_from_shell()
    if not wm and age is None:
        wm = _focused_wm_class_x11()
    return profile_from_wm_class(wm), age


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


class FocusState:
    """What the previous poll saw."""

    def __init__(self) -> None:
        self.profile = ""
        self.suppressed: set[str] = set()


def tick(state: FocusState) -> None:
    """One poll. Wake on focus *enter* into a paused profile.

    Not woken: a profile paused while focused (until focus leaves it), and one
    that got focus because of a bare workspace switch (not a window pick).
    """
    cur, switch_age = focused_docked_profile()
    prev = state.profile
    if prev and prev != cur:
        state.suppressed.discard(prev)
    if cur and container_paused(cur):
        if cur != prev:
            if switch_age is not None and switch_age <= WORKSPACE_SWITCH_S:
                _log(f"not waking {cur}: focused by workspace switch")
                state.suppressed.add(cur)
            elif cur not in state.suppressed:
                request_wake(cur)
        else:
            # Still focused when it became / stayed paused — wait until focus leaves.
            state.suppressed.add(cur)
    state.profile = cur


def watch() -> None:
    _log("watching focus (GNOME Focused Window / Dash raise)")
    state = FocusState()
    while True:
        try:
            tick(state)
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
    """Start the focus watcher if none is running.

    Runs the Rust watcher (``wake-paused/``, built by ``docked-browser build``)
    when its binary exists; this Python watcher is the fallback. Both take the
    same lock, so only one ever runs.
    """
    env = desktop_env()
    if RUST_WATCHER.is_file() and os.access(RUST_WATCHER, os.X_OK):
        subprocess.Popen(
            [str(RUST_WATCHER), "--cli", str(CLI)],
            env=env,
            cwd=str(GUI_DIR),
            start_new_session=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return
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
