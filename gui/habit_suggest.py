#!/usr/bin/env python3
"""Weekday-and-hour profile suggestion.

Counts real use (focused docked window, or ``mark_used`` from Open / Resume /
activate). It does not train LinUCB and it does not share the pause-warning
click. Checkpoint: ``~/.local/share/docked-browser/habit.json``.

A slot is the Malaysia weekday (Monday=1 … Sunday=7) and hour (0–23). The
window is the current ISO week plus the three weeks before it. Suggest the
profile that appears in strictly the most of those weeks, and only when that
count is at least 3. One notification per slot per Malaysia day. Click runs
``bin/docked-browser activate <profile>`` and returns immediately.

The profile panel uses the same winner (``slot_winner``). It hides that row
when the profile is already running. That hide is not the notification's
already-focused skip.
"""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import threading
import time
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Callable

from predict_sleep import CLI, MYT_OFFSET_SECS, PROFILE_RE, STATE_DIR

HABIT_PATH = STATE_DIR / "habit.json"
WEEKS_TO_KEEP = 8
WINDOW_WEEKS = 4
MIN_WEEKS = 3
SUGGEST_WAIT_S = 5 * 60
_CLASS_RE = re.compile(r"docked-browser-([a-z0-9_-]{1,64})")
_OPEN_ACTIONS = frozenset({"default", "open"})


def myt_datetime(epoch: float | None = None) -> datetime:
    """Malaysia civil time, stored as a UTC-labeled datetime of those fields."""
    if epoch is None:
        epoch = time.time()
    return datetime.fromtimestamp(epoch + MYT_OFFSET_SECS, tz=timezone.utc)


def week_key(when: datetime) -> str:
    iso = when.isocalendar()
    return f"{iso.year}-W{iso.week:02d}"


def slot_key(when: datetime) -> str:
    return f"{when.isocalendar().weekday}-{when.hour:02d}"


def recent_week_keys(when: datetime, n: int = WINDOW_WEEKS) -> list[str]:
    """Current ISO week first, then the preceding weeks."""
    keys: list[str] = []
    seen: set[str] = set()
    for i in range(n):
        key = week_key(when - timedelta(days=7 * i))
        if key not in seen:
            seen.add(key)
            keys.append(key)
    return keys


def profile_from_wm_class(wm: str) -> str:
    """Lowercased profile from a focused ``WM_CLASS``, or ''."""
    match = _CLASS_RE.search((wm or "").lower())
    return match.group(1) if match else ""


def notification_requests_open(signal_name: str, detail: object) -> bool:
    """Body / Open activates. Close, expiry, and other signals do not."""
    if signal_name == "ActionInvoked":
        return detail in _OPEN_ACTIONS
    if signal_name == "ActivationToken":
        return isinstance(detail, str) and bool(detail.strip())
    return False


def record_use(data: dict[str, Any], profile: str, when: datetime) -> bool:
    """Mark ``profile`` present in this week and slot. False if already there."""
    if not isinstance(profile, str) or not PROFILE_RE.match(profile):
        return False
    weeks = data.setdefault("weeks", {})
    week = weeks.setdefault(week_key(when), {})
    slot = slot_key(when)
    names = week.get(slot)
    if not isinstance(names, list):
        names = []
    if profile in names:
        week[slot] = names
        return False
    names.append(profile)
    names.sort()
    week[slot] = names
    return True


def slot_winner(data: dict[str, Any], when: datetime) -> str | None:
    """Profile used in this slot in the most weeks, or None.

    At least ``MIN_WEEKS`` of the last ``WINDOW_WEEKS``, and no tie.
    Does not apply the notification skips (already offered today, already focused).
    """
    slot = slot_key(when)
    counts: dict[str, int] = {}
    weeks = data.get("weeks") or {}
    for key in recent_week_keys(when, WINDOW_WEEKS):
        names = (weeks.get(key) or {}).get(slot) or []
        if not isinstance(names, list):
            continue
        seen: set[str] = set()
        for name in names:
            if not isinstance(name, str) or name in seen or not PROFILE_RE.match(name):
                continue
            seen.add(name)
            counts[name] = counts.get(name, 0) + 1
    if not counts:
        return None
    best = max(counts.values())
    if best < MIN_WEEKS:
        return None
    winners = [name for name, count in counts.items() if count == best]
    if len(winners) != 1:
        return None
    return winners[0]


def pick_suggestion(data: dict[str, Any], when: datetime, *, focused: str = "") -> str | None:
    """Winning profile for this slot, or None when the slot is not consistent."""
    day = when.date().isoformat()
    slot = slot_key(when)
    suggested = (data.get("suggested") or {}).get(day) or []
    if isinstance(suggested, list) and slot in suggested:
        return None
    winner = slot_winner(data, when)
    if not winner:
        return None
    if focused and winner.casefold() == focused.casefold():
        return None
    return winner


def for_now_profile(
    data: dict[str, Any],
    when: datetime,
    *,
    resumed: bool = False,
) -> str | None:
    """Profile to feature in the panel, or None.

    Same winner as ``slot_winner``. Hidden when that profile is already
    resumed (running, not paused). A tie or a short tally shows nothing,
    and a resumed winner is not replaced by the runner-up.
    """
    winner = slot_winner(data, when)
    if not winner or resumed:
        return None
    return winner


def panel_for_now(
    running: set[str],
    *,
    path: Path | None = None,
    when: datetime | None = None,
) -> str | None:
    """Read the habit file and return the profile to feature, or None.

    ``running`` is profiles whose container is resumed (docker state running,
    not paused). This does not mark the slot as suggested.
    """
    when = when or myt_datetime()
    data = _load(path or HABIT_PATH)
    winner = slot_winner(data, when)
    if not winner:
        return None
    return for_now_profile(data, when, resumed=winner in running)


def mark_suggested(data: dict[str, Any], when: datetime) -> None:
    day = when.date().isoformat()
    slot = slot_key(when)
    suggested = data.setdefault("suggested", {})
    slots = suggested.get(day)
    if not isinstance(slots, list):
        slots = []
    if slot not in slots:
        slots.append(slot)
        slots.sort()
    suggested[day] = slots


def clear_suggested(data: dict[str, Any], when: datetime) -> None:
    day = when.date().isoformat()
    slot = slot_key(when)
    suggested = data.get("suggested")
    if not isinstance(suggested, dict):
        return
    slots = suggested.get(day)
    if not isinstance(slots, list):
        return
    suggested[day] = [item for item in slots if item != slot]
    if not suggested[day]:
        del suggested[day]


def prune_habit(data: dict[str, Any], now: datetime, *, keep: int = WEEKS_TO_KEEP) -> None:
    iso = now.isocalendar()
    current_monday = date.fromisocalendar(iso.year, iso.week, 1)
    oldest = current_monday - timedelta(weeks=max(1, keep) - 1)
    weeks = data.get("weeks")
    if isinstance(weeks, dict):
        for key in list(weeks):
            monday = _parse_week(key)
            if monday is None or monday < oldest:
                del weeks[key]
    suggested = data.get("suggested")
    if isinstance(suggested, dict):
        cutoff = now.date() - timedelta(weeks=keep)
        for day in list(suggested):
            try:
                parsed = date.fromisoformat(day)
            except ValueError:
                del suggested[day]
                continue
            if parsed < cutoff:
                del suggested[day]


def log_use(profile: str, *, when: datetime | None = None, path: Path | None = None) -> bool:
    """Persist one real use. Idle background time must not call this."""
    if not isinstance(profile, str) or not PROFILE_RE.match(profile):
        return False
    when = when or myt_datetime()
    path = path or HABIT_PATH

    def mutate(data: dict[str, Any]) -> bool:
        return record_use(data, profile, when)

    return bool(_update(path, mutate))


def maybe_suggest(
    *,
    focused: str | None = None,
    path: Path | None = None,
    notify: Callable[[str], bool] | None = None,
    when: datetime | None = None,
) -> str | None:
    """Show the suggestion once. Returns the profile name when it was offered.

    ``notify`` defaults to a daemon thread that posts a notification and
    returns without waiting for the click. A false result drops the slot
    marker so the next poll can try again.
    """
    path = path or HABIT_PATH
    when = when or myt_datetime()
    if focused is None:
        focused = detect_focused_profile()
        if focused is None:
            return None

    def choose(data: dict[str, Any]) -> str | None:
        name = pick_suggestion(data, when, focused=focused or "")
        if not name:
            return None
        mark_suggested(data, when)
        return name

    try:
        chosen = _update(path, choose)
    except OSError as exc:
        _ps_log(f"habit suggest failed: {exc}")
        return None
    if not chosen:
        return None

    if notify is None:
        threading.Thread(
            target=_notify_worker,
            args=(chosen, when, path),
            name="habit-suggest",
            daemon=True,
        ).start()
        _ps_log(f"suggest {chosen} {slot_key(when)}")
        return chosen

    try:
        ok = bool(notify(chosen))
    except Exception as exc:  # noqa: BLE001 — a bad notifier must not stick the slot
        _ps_log(f"habit notify failed: {exc}")
        ok = False
    if not ok:
        _clear_slot(path, when)
        return None
    _ps_log(f"suggest {chosen} {slot_key(when)}")
    return chosen


def daemon_tick() -> None:
    """One habit sample from the predict-sleep loop. Never pauses or trains."""
    try:
        from predict_sleep import (
            _window_pid_and_class,
            docker_running_profiles,
            focused_docked_profile,
        )

        exact = focused_docked_profile(docker_running_profiles()) or ""
        by_class = ""
        if not exact:
            _pid, wm = _window_pid_and_class()
            by_class = profile_from_wm_class(wm)
    except Exception as exc:  # noqa: BLE001
        _ps_log(f"habit focus error: {exc}")
        return
    if exact:
        try:
            log_use(exact)
        except OSError as exc:
            _ps_log(f"habit log failed: {exc}")
    # Class covers a paused window that is still the focused one.
    maybe_suggest(focused=exact or by_class)


def detect_focused_profile() -> str | None:
    """Focused profile name, '' if another window, None if focus is unknown."""
    try:
        from predict_sleep import (
            _window_pid_and_class,
            docker_running_profiles,
            focused_docked_profile,
        )

        exact = focused_docked_profile(docker_running_profiles()) or ""
        if exact:
            return exact
        _pid, wm = _window_pid_and_class()
        return profile_from_wm_class(wm)
    except Exception:  # noqa: BLE001
        return None


def spawn_activate(profile: str, *, token: str = "", popen: Callable[..., Any] = subprocess.Popen) -> bool:
    """Run ``activate`` and return. Does not mark the click as a use."""
    if not isinstance(profile, str) or not PROFILE_RE.match(profile):
        return False
    if not CLI.is_file():
        return False
    env = os.environ.copy()
    safe = _safe_token(token)
    if safe:
        env["DOCKED_ACTIVATION_TOKEN"] = safe
    popen(
        [str(CLI), "activate", profile],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
        close_fds=True,
    )
    return True


def _notify_worker(profile: str, when: datetime, path: Path) -> None:
    shown = False
    try:
        shown = _post_and_wait(profile)
    except Exception as exc:  # noqa: BLE001
        _ps_log(f"habit notify failed: {exc}")
        shown = False
    if not shown:
        _clear_slot(path, when)


def _clear_slot(path: Path, when: datetime) -> None:
    try:
        _update(path, lambda data: clear_suggested(data, when))
    except OSError as exc:
        _ps_log(f"habit unmark failed: {exc}")


_gi_warned = False
_bus_warned = False


def _post_and_wait(profile: str) -> bool:
    """Post the suggestion and wait for a click or timeout. True if it was shown."""
    global _gi_warned
    try:
        import gi

        gi.require_version("Gio", "2.0")
        gi.require_version("GLib", "2.0")
        from gi.repository import Gio, GLib
    except Exception as exc:  # noqa: BLE001
        if not _gi_warned:
            _gi_warned = True
            _ps_log(f"habit notifications unavailable ({exc})")
        return False

    ctx = GLib.MainContext.new()
    ctx.push_thread_default()
    try:
        return _post_on_context(profile, ctx, GLib, Gio)
    finally:
        ctx.pop_thread_default()


def _post_on_context(profile: str, ctx: Any, GLib: Any, Gio: Any) -> bool:
    global _bus_warned
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    flags = (
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    )
    try:
        if not address:
            address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, None)
        bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    except Exception as exc:  # noqa: BLE001
        if not _bus_warned:
            _bus_warned = True
            _ps_log(f"habit notification bus unavailable ({exc})")
        return False

    loop = GLib.MainLoop.new(ctx, False)
    state: dict[str, Any] = {"nid": 0, "done": False, "opened": False, "token": ""}

    def _finish() -> None:
        if state["done"]:
            return
        state["done"] = True
        loop.quit()

    def _same(nid: object) -> bool:
        try:
            return int(nid) == int(state["nid"]) and int(state["nid"]) != 0
        except (TypeError, ValueError):
            return False

    def on_signal(_conn, _sender, _path, _iface, signal, params, *_user):
        try:
            nid, detail = params.unpack()
        except Exception:  # noqa: BLE001
            return
        if not _same(nid):
            return
        if signal == "ActivationToken" and isinstance(detail, str):
            state["token"] = detail
        if notification_requests_open(signal, detail):
            if not state["opened"]:
                state["opened"] = True
                token = state["token"] or (detail if signal == "ActivationToken" else "")
                try:
                    if spawn_activate(profile, token=token if isinstance(token, str) else ""):
                        _ps_log(f"suggest open {profile}")
                except Exception as exc:  # noqa: BLE001
                    _ps_log(f"suggest open failed: {exc}")
            _finish()
        elif signal == "NotificationClosed":
            # Expired, dismissed, or closed by us. Not a use and not an open.
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
    proxy = Gio.DBusProxy.new_sync(
        bus,
        Gio.DBusProxyFlags.NONE,
        None,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        None,
    )
    def _on_timeout(*_args: object) -> bool:
        _finish()
        return GLib.SOURCE_REMOVE

    source = GLib.timeout_source_new_seconds(SUGGEST_WAIT_S)
    source.set_callback(_on_timeout)
    source.attach(ctx)
    posted = False
    try:
        nid = proxy_notify(Gio, GLib, proxy, profile)
        state["nid"] = int(nid)
        if state["nid"] == 0:
            return False
        posted = True
        loop.run()
        return True
    except Exception as exc:  # noqa: BLE001
        _ps_log(f"habit Notify failed ({exc})")
        return posted
    finally:
        try:
            bus.signal_unsubscribe(sub_id)
        except Exception:  # noqa: BLE001
            pass
        try:
            if state["nid"]:
                proxy.call_sync(
                    "CloseNotification",
                    GLib.Variant("(u)", (state["nid"],)),
                    Gio.DBusCallFlags.NONE,
                    2000,
                    None,
                )
        except Exception:  # noqa: BLE001
            pass
        try:
            bus.close_sync(None)
        except Exception:  # noqa: BLE001
            pass


def proxy_notify(Gio: Any, GLib: Any, proxy: Any, profile: str) -> int:
    """Freedesktop Notify. Separate hints and actions from the pause warning."""
    reply = proxy.call_sync(
        "Notify",
        GLib.Variant(
            "(susssasa{sv}i)",
            (
                "Docked Browser",
                0,
                "web-browser",
                f"Open “{profile}”?",
                "You usually use this profile around this time.",
                ["default", "Open", "open", "Open"],
                {"suppress-sound": GLib.Variant("b", True)},
                SUGGEST_WAIT_S * 1000,
            ),
        ),
        Gio.DBusCallFlags.NONE,
        5000,
        None,
    )
    return int(reply.unpack()[0])


def _parse_week(key: object) -> date | None:
    if not isinstance(key, str):
        return None
    match = re.match(r"^(\d{4})-W(\d{2})$", key)
    if not match:
        return None
    try:
        return date.fromisocalendar(int(match.group(1)), int(match.group(2)), 1)
    except ValueError:
        return None


def _load(path: Path) -> dict[str, Any]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {"weeks": {}, "suggested": {}}
    if not isinstance(raw, dict):
        return {"weeks": {}, "suggested": {}}
    weeks = raw.get("weeks")
    suggested = raw.get("suggested")
    return {
        "weeks": weeks if isinstance(weeks, dict) else {},
        "suggested": suggested if isinstance(suggested, dict) else {},
    }


def _write(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"weeks": data.get("weeks") or {}, "suggested": data.get("suggested") or {}}
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def _dump(data: dict[str, Any]) -> str:
    return json.dumps(
        {"weeks": data.get("weeks") or {}, "suggested": data.get("suggested") or {}},
        sort_keys=True,
    )


def _update(path: Path, fn: Callable[[dict[str, Any]], Any]) -> Any:
    path.parent.mkdir(parents=True, exist_ok=True)
    lock_path = Path(str(path) + ".lock")
    with lock_path.open("a+", encoding="utf-8") as lockf:
        fcntl.flock(lockf.fileno(), fcntl.LOCK_EX)
        try:
            data = _load(path)
            before = _dump(data)
            result = fn(data)
            prune_habit(data, myt_datetime())
            if _dump(data) != before:
                _write(path, data)
            return result
        finally:
            fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)


def _safe_token(token: str) -> str:
    if not isinstance(token, str):
        return ""
    token = token.strip()
    if not token or len(token) > 512 or any(ch in token for ch in "\n\r\x00"):
        return ""
    return token


def _ps_log(line: str) -> None:
    try:
        from predict_sleep import _log

        _log(line)
    except Exception:  # noqa: BLE001
        print(f"[predict-sleep] {line}", flush=True)
