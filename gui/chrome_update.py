#!/usr/bin/env python3
"""Check Google Chrome Debian repository for new versions."""

from __future__ import annotations

import gzip
import json
import os
import re
import shutil
import subprocess
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

PACKAGES_URL = "https://dl.google.com/linux/chrome/deb/dists/stable/main/binary-amd64/Packages.gz"
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
UPDATE_STATE_FILE = STATE_DIR / "chrome-update-check.json"
CHECK_INTERVAL_S = 86400  # Check once per day (24h)
RETRY_AFTER_FAIL_S = 3600  # Retry a failed probe sooner than the daily check
MYT_OFFSET_SECS = 8 * 3600
IMAGE_NAME = "local-docked-browser"
LEGACY_IMAGE_NAME = "local-chrome-cockpit"
_BIN = Path(__file__).resolve().parent.parent / "bin"
_CLI = _BIN / "docked-browser"
_START_GUI = _BIN / "start-gui"
_notify_lock = threading.Lock()
_notify_inflight = False


def parse_packages_data(data: str) -> str | None:
    """Extract google-chrome-stable Version string from Debian Packages text."""
    for block in data.split("\n\n"):
        if "Package: google-chrome-stable" in block:
            for line in block.splitlines():
                if line.startswith("Version:"):
                    return line.split(":", 1)[1].strip()
    return None


def fetch_upstream_version(timeout: float = 10.0) -> str | None:
    """Fetch latest google-chrome-stable version from Google's repo."""
    try:
        req = urllib.request.Request(
            PACKAGES_URL,
            headers={"User-Agent": "DockedBrowser/1.0 (Debian)"},
        )
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            compressed = resp.read()
            decompressed = gzip.decompress(compressed).decode("utf-8", errors="replace")
            return parse_packages_data(decompressed)
    except (urllib.error.URLError, OSError, ValueError, Exception):
        return None


def get_installed_chrome_version() -> str | None:
    """Inspect local docker image to get installed Google Chrome version."""
    for img in (IMAGE_NAME, LEGACY_IMAGE_NAME):
        try:
            inspect = subprocess.run(
                ["docker", "image", "inspect", img],
                capture_output=True,
                text=True,
                timeout=10,
            )
            if inspect.returncode != 0:
                continue
            res = subprocess.run(
                ["docker", "run", "--rm", img, "--version"],
                capture_output=True,
                text=True,
                timeout=15,
            )
            if res.returncode == 0:
                # e.g. "Google Chrome 153.0.8010.52 \n"
                out = res.stdout.strip()
                match = re.search(r"(\d+(?:\.\d+)+)", out)
                if match:
                    return match.group(1)
                return out
        except (OSError, subprocess.TimeoutExpired):
            continue
    return None


def _normalize_version_tuple(ver_str: str) -> tuple[int, ...]:
    """Chrome version only. Debian revision (-1) and epoch are ignored.

    ``google-chrome --version`` prints ``155.0.8059.84``. The Debian repo
    prints ``155.0.8059.84-1``. Those are the same Chrome.
    """
    ver = ver_str.strip()
    if ":" in ver:
        ver = ver.split(":", 1)[1]
    ver = ver.split("-", 1)[0]
    parts = []
    for part in ver.split("."):
        if part.isdigit():
            parts.append(int(part))
    return tuple(parts)


def is_newer_version(upstream: str, installed: str) -> bool:
    """Return True if upstream version is strictly newer than installed version."""
    v_up = _normalize_version_tuple(upstream)
    v_inst = _normalize_version_tuple(installed)
    if not v_up or not v_inst:
        return False
    return v_up > v_inst


def load_update_state() -> dict:
    if UPDATE_STATE_FILE.is_file():
        try:
            return json.loads(UPDATE_STATE_FILE.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            pass
    return {}


def save_update_state(state: dict) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    tmp = UPDATE_STATE_FILE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    tmp.replace(UPDATE_STATE_FILE)


def _check_due(state: dict, now: float, force: bool) -> bool:
    if force or not state or "update_available" not in state:
        return True
    last = float(state.get("last_checked_at") or 0)
    interval = RETRY_AFTER_FAIL_S if state.get("check_failed") else CHECK_INTERVAL_S
    return now - last >= interval


def _stamp(state: dict, now: float) -> dict:
    state["last_checked_at"] = now
    state["last_checked_iso"] = datetime.now(timezone.utc).isoformat()
    return state


def check_for_chrome_update(force: bool = False) -> dict:
    """Perform check if interval has passed (or forced). Return update info dict."""
    state = load_update_state()
    now = time.time()

    if not _check_due(state, now, force):
        return state

    probed_installed = get_installed_chrome_version()
    probed_upstream = fetch_upstream_version()
    installed = probed_installed or state.get("installed_version")
    upstream = probed_upstream or state.get("latest_version")
    notified = state.get("last_notified_at")

    if not probed_installed or not probed_upstream:
        # A dead network or a missing image must not clear a known update.
        if installed and upstream:
            kept = dict(state)
            kept["installed_version"] = installed
            kept["latest_version"] = upstream
            if probed_installed or probed_upstream:
                kept["update_available"] = is_newer_version(str(upstream), str(installed))
            kept["check_failed"] = True
            _stamp(kept, now)
            try:
                save_update_state(kept)
            except OSError:
                pass
            return kept
        failed = _stamp(
            {
                "installed_version": installed,
                "latest_version": upstream,
                "update_available": False,
                "check_failed": True,
            },
            now,
        )
        if notified:
            failed["last_notified_at"] = notified
        try:
            save_update_state(failed)
        except OSError:
            pass
        return failed

    fresh = _stamp(
        {
            "installed_version": installed,
            "latest_version": upstream,
            "update_available": is_newer_version(str(upstream), str(installed)),
        },
        now,
    )
    if notified:
        fresh["last_notified_at"] = notified
    try:
        save_update_state(fresh)
    except OSError:
        pass
    return fresh


def _spawn(cmd: list[str]) -> None:
    subprocess.Popen(
        cmd,
        cwd=str(_BIN.parent),
        start_new_session=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _open_gui() -> None:
    if _START_GUI.is_file():
        _spawn([str(_START_GUI)])
    url = "http://127.0.0.1:8787/"
    if shutil.which("xdg-open"):
        _spawn(["xdg-open", url])
    else:
        _spawn(["gio", "open", url])


def _present_update_notification(latest: str | None, installed: str | None) -> None:
    """Post the notice on this thread's GLib context. Update installs; a click opens the app."""
    global _notify_inflight
    try:
        import gi

        gi.require_version("Gio", "2.0")
        gi.require_version("GLib", "2.0")
        from gi.repository import Gio, GLib
    except Exception:
        with _notify_lock:
            _notify_inflight = False
        return

    ctx = GLib.MainContext.new()
    ctx.push_thread_default()
    try:
        _present_on_context(ctx, Gio, GLib, latest, installed)
    finally:
        ctx.pop_thread_default()
        with _notify_lock:
            _notify_inflight = False


def _present_on_context(ctx, Gio, GLib, latest: str | None, installed: str | None) -> None:
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    flags = (
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    )
    try:
        if not address:
            address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, None)
        bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    except Exception:
        return

    loop = GLib.MainLoop.new(ctx, False)
    chosen: dict[str, str] = {"action": ""}
    posted: dict[str, int] = {"nid": 0}

    def _finish() -> None:
        if loop.is_running():
            loop.quit()

    def _same(got: object) -> bool:
        try:
            return int(got) == posted["nid"] and posted["nid"] != 0
        except (TypeError, ValueError):
            return False

    def on_signal(_conn, _sender, _path, _iface, signal, params, *_user):
        try:
            got, detail = params.unpack()
        except Exception:
            return
        if not _same(got):
            return
        if signal == "ActionInvoked" and not chosen["action"]:
            # "update" is the button. "default" is a click on the notification.
            if detail == "update":
                chosen["action"] = "build"
            elif detail == "default":
                chosen["action"] = "open"
            else:
                return
            _finish()
        elif signal == "NotificationClosed":
            _finish()

    sub_id = bus.signal_subscribe(
        "org.freedesktop.Notifications",
        "org.freedesktop.Notifications",
        None,
        "/org/freedesktop/Notifications",
        None,
        Gio.DBusSignalFlags.NONE,
        on_signal,
        None,
    )
    try:
        proxy = Gio.DBusProxy.new_sync(
            bus,
            Gio.DBusProxyFlags.NONE,
            None,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            None,
        )
        ver_text = f"Chrome {latest} is available" if latest else "New Chrome update available"
        curr_text = f" (installed: {installed})" if installed else ""
        nid = proxy.call_sync(
            "Notify",
            GLib.Variant(
                "(susssasa{sv}i)",
                (
                    "Docked Browser",
                    0,
                    "software-update-available",
                    ver_text,
                    f"A new version of Google Chrome is available{curr_text}.",
                    ["default", "Open", "update", "Update"],
                    {"suppress-sound": GLib.Variant("b", True)},
                    0,
                ),
            ),
            Gio.DBusCallFlags.NONE,
            5000,
            None,
        ).unpack()[0]
        posted["nid"] = int(nid)
        if not posted["nid"]:
            return
        state = load_update_state()
        state["last_notified_at"] = time.time()
        try:
            save_update_state(state)
        except OSError:
            pass
        loop.run()
    except Exception:
        return
    finally:
        try:
            bus.signal_unsubscribe(sub_id)
        except Exception:
            pass
        try:
            bus.close_sync(None)
        except Exception:
            pass

    if chosen["action"] == "build" and _CLI.is_file():
        _spawn([str(_CLI), "build"])
    elif chosen["action"] == "open":
        _open_gui()


def notify_chrome_update_if_needed(update_info: dict | None = None) -> bool:
    """Show a desktop notification if an update is available (once per day)."""
    global _notify_inflight
    info = update_info or check_for_chrome_update()
    if not info.get("update_available"):
        return False

    state = load_update_state()
    now = time.time()
    last_notified = float(state.get("last_notified_at") or 0)
    if now - last_notified < CHECK_INTERVAL_S:
        return False

    with _notify_lock:
        if _notify_inflight:
            return False
        _notify_inflight = True

    threading.Thread(
        target=_present_update_notification,
        args=(info.get("latest_version"), info.get("installed_version")),
        name="chrome-update-notify",
        daemon=True,
    ).start()
    return True


def update_status_payload() -> dict:
    """Returns current update status; triggers check if never checked or outdated."""
    state = load_update_state()
    if _check_due(state, time.time(), force=False):
        return check_for_chrome_update()
    return state
