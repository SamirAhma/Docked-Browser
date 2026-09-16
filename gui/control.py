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
PREFS_FILE = STATE_DIR / "prefs.json"
# Unified brand family (see gui/brand_icons.py) — same mark, different glyphs
LAUNCHER_ICON = ICON_DIR / "docked-browser.png"  # web UI / generic
TRAY_ICON = ICON_DIR / "docked-browser-tray.png"
MODAL_ICON = ICON_DIR / "docked-browser-modal.png"
BRAND_ICONS_PY = Path(__file__).resolve().parent / "brand_icons.py"

THEMES = frozenset({"light", "dark", "system"})
DEFAULT_PREFS = {"theme": "system"}

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


def detect_system_theme() -> str:
    """Return light|dark from the desktop (GNOME color-scheme / GTK)."""
    try:
        result = subprocess.run(
            ["gsettings", "get", "org.gnome.desktop.interface", "color-scheme"],
            capture_output=True,
            text=True,
            timeout=2,
        )
        if result.returncode == 0:
            value = result.stdout.strip().strip("'\"")
            if value == "prefer-dark":
                return "dark"
            if value in {"prefer-light", "default"}:
                return "light"
    except (OSError, subprocess.TimeoutExpired):
        pass

    try:
        result = subprocess.run(
            ["gsettings", "get", "org.gnome.desktop.interface", "gtk-theme"],
            capture_output=True,
            text=True,
            timeout=2,
        )
        if result.returncode == 0 and "dark" in result.stdout.strip().lower():
            return "dark"
    except (OSError, subprocess.TimeoutExpired):
        pass

    try:
        import gi

        gi.require_version("Gtk", "3.0")
        from gi.repository import Gtk

        settings = Gtk.Settings.get_default()
        if settings is not None and settings.get_property("gtk-application-prefer-dark-theme"):
            return "dark"
    except Exception:
        pass

    return "light"


def load_prefs() -> dict:
    """User prefs shared by web UI, modal, and tray (theme, …)."""
    data = dict(DEFAULT_PREFS)
    try:
        raw = json.loads(PREFS_FILE.read_text(encoding="utf-8"))
        if isinstance(raw, dict):
            data.update(raw)
    except (OSError, json.JSONDecodeError):
        pass
    theme = str(data.get("theme", "system")).lower()
    data["theme"] = theme if theme in THEMES else "system"
    return data


def save_prefs(updates: dict) -> dict:
    data = load_prefs()
    if "theme" in updates:
        theme = str(updates["theme"]).lower()
        if theme not in THEMES:
            raise ValueError("theme must be light, dark, or system")
        data["theme"] = theme
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    PREFS_FILE.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return prefs_payload_from(data)


def get_theme_pref() -> str:
    """Stored preference: light | dark | system."""
    return load_prefs()["theme"]


def resolve_theme(pref: str | None = None) -> str:
    """Effective appearance: light | dark."""
    choice = (pref or get_theme_pref()).lower()
    if choice == "dark":
        return "dark"
    if choice == "light":
        return "light"
    return detect_system_theme()


def get_theme() -> str:
    """Effective theme for painting UIs (resolves system → light/dark)."""
    return resolve_theme()


def prefs_payload_from(data: dict) -> dict:
    pref = str(data.get("theme", "system"))
    if pref not in THEMES:
        pref = "system"
    return {"theme": pref, "theme_resolved": resolve_theme(pref)}


def prefs_payload() -> dict:
    return prefs_payload_from(load_prefs())


def ensure_brand_icons() -> None:
    """Generate web/tray/modal brand PNGs if missing."""
    needed = (LAUNCHER_ICON, TRAY_ICON, MODAL_ICON)
    if all(p.is_file() for p in needed):
        return
    if not BRAND_ICONS_PY.is_file():
        return
    try:
        subprocess.run(
            ["python3", str(BRAND_ICONS_PY), "--icons-dir", str(ICON_DIR), "--no-static"],
            check=False,
            capture_output=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass


def playing_tray_icon() -> Path:
    """Tray status icon — brand tray glyph (not a profile dock icon)."""
    ensure_brand_icons()
    return TRAY_ICON if TRAY_ICON.is_file() else LAUNCHER_ICON


def modal_window_icon() -> Path:
    """Focus modal window icon — brand modal glyph."""
    ensure_brand_icons()
    return MODAL_ICON if MODAL_ICON.is_file() else LAUNCHER_ICON


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
