#!/usr/bin/env python3
"""Shared helpers for Docked Browser focus modal + tray."""
from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path

PROFILE_RE = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")
PROFILES_DIR = Path.home() / ".config" / "docker-chrome-profiles"
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
ACTIVE_FILE = STATE_DIR / "active-profile"
ICON_DIR = STATE_DIR / "icons"
LAUNCHER_ICON = ICON_DIR / "docked-browser.png"

BIN_DIR = Path(__file__).resolve().parent.parent / "bin"
CLI = BIN_DIR / "docked-browser"
FOCUS_MODAL = BIN_DIR / "focus-modal"
START_GUI = BIN_DIR / "start-gui"
ROOT = BIN_DIR.parent


def profile_icon_path(name: str) -> Path | None:
    """Same image the Dash uses for this profile (installed dock PNG, else custom upload)."""
    if not PROFILE_RE.match(name):
        return None
    installed = ICON_DIR / f"docked-browser-{name}.png"
    if installed.is_file():
        return installed
    custom = PROFILES_DIR / name / "dock-icon.png"
    if custom.is_file():
        return custom
    return None


def desktop_env() -> dict[str, str]:
    env = os.environ.copy()
    uid = os.getuid()
    runtime = env.get("XDG_RUNTIME_DIR") or f"/run/user/{uid}"
    env.setdefault("XDG_RUNTIME_DIR", runtime)
    env.setdefault("DISPLAY", ":0")
    if not env.get("WAYLAND_DISPLAY") and Path(runtime, "wayland-0").exists():
        env["WAYLAND_DISPLAY"] = "wayland-0"
    return env


def docker_rows() -> dict[str, dict]:
    try:
        result = subprocess.run(
            [
                "docker",
                "ps",
                "-a",
                "--filter",
                "name=chrome-",
                "--format",
                "{{.Names}}\t{{.Status}}\t{{.State}}",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {}

    out: dict[str, dict] = {}
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        name, status, state = parts[0], parts[1], parts[2]
        if not name.startswith("chrome-"):
            continue
        profile = name[len("chrome-") :]
        if not PROFILE_RE.match(profile):
            continue
        out[profile] = {
            "name": name,
            "profile": profile,
            "status": status,
            "state": state,
            "paused": state == "paused",
            "running": state == "running",
        }
    return out


def known_profiles() -> list[str]:
    profiles: set[str] = set()
    if PROFILES_DIR.is_dir():
        for path in PROFILES_DIR.iterdir():
            if path.is_dir() and PROFILE_RE.match(path.name):
                profiles.add(path.name)
    profiles.update(docker_rows())
    return sorted(profiles)


def profile_state(name: str) -> str:
    row = docker_rows().get(name)
    if not row:
        return "stopped"
    if row["paused"]:
        return "paused"
    if row["running"]:
        return "running"
    return "stopped"


def list_profiles(*, playing_only: bool = False) -> list[dict]:
    rows = docker_rows()
    active = get_active_profile()
    items = []
    for name in known_profiles():
        row = rows.get(name)
        if row and row["paused"]:
            state = "paused"
        elif row and row["running"]:
            state = "running"
        else:
            state = "stopped"
        if playing_only and state != "running":
            continue
        custom = profile_icon_path(name)
        items.append(
            {
                "profile": name,
                "state": state,
                "active": name == active,
                "icon": str(custom) if custom else "",
            }
        )
    # Active first, then name
    items.sort(key=lambda i: (0 if i["active"] else 1, i["profile"]))
    return items


def list_playing_profiles() -> list[dict]:
    """Only Docker containers currently running (not paused/stopped)."""
    return list_profiles(playing_only=True)


def playing_tray_icon() -> Path:
    """Icon for the tray: same dock image as the modal uses for the playing profile."""
    playing = list_playing_profiles()
    for item in playing:
        if item.get("active"):
            path = profile_icon_path(item["profile"])
            if path:
                return path
    if playing:
        path = profile_icon_path(playing[0]["profile"])
        if path:
            return path
    return LAUNCHER_ICON


def get_active_profile() -> str:
    try:
        name = ACTIVE_FILE.read_text(encoding="utf-8").strip()
    except OSError:
        return ""
    return name if PROFILE_RE.match(name) else ""


def set_active_profile(name: str) -> None:
    if not PROFILE_RE.match(name):
        return
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    ACTIVE_FILE.write_text(name + "\n", encoding="utf-8")


def run_cli(action: str, profile: str | None = None) -> dict:
    if not CLI.is_file():
        return {"ok": False, "output": f"Missing {CLI}"}
    cmd = [str(CLI), action]
    if profile is not None:
        if not PROFILE_RE.match(profile):
            return {"ok": False, "output": "Invalid profile name"}
        cmd.append(profile)
    try:
        result = subprocess.run(
            cmd,
            cwd=str(ROOT),
            env=desktop_env(),
            capture_output=True,
            text=True,
            timeout=120,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return {"ok": False, "output": str(exc)}
    output = (result.stdout or "") + (result.stderr or "")
    if result.returncode == 0 and profile and action in {"run", "activate", "resume"}:
        set_active_profile(profile)
    return {"ok": result.returncode == 0, "output": output.strip() or "(no output)"}


def open_focus_modal() -> None:
    subprocess.Popen([str(FOCUS_MODAL)], cwd=str(ROOT), env=desktop_env(), start_new_session=True)


def open_full_gui() -> None:
    subprocess.Popen([str(START_GUI)], cwd=str(ROOT), env=desktop_env(), start_new_session=True)


def status_json() -> str:
    return json.dumps(
        {
            "active": get_active_profile(),
            "profiles": list_profiles(),
        },
        indent=2,
    )
