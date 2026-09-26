#!/usr/bin/env python3
"""Per-profile predict-sleep — LinUCB formula ported from ambient-sleep.

Maps ambient-sleep StayAwake / AllowSleep onto Chrome profiles:
  KeepRunning  → leave the container running
  Pause        → ``docker pause`` when the profile looks unused

Activity is the focused Chrome window (its PID belongs to that container),
plus host keyboard/mouse idle. Container CPU is not a use signal — a page
you are reading often sits near 0% while background tabs sit higher.

Tick order (same as ambient-sleep; do not reorder):
  1. Sample fused idle for each *running* profile
  2. Credit the previous decision (delayed reward + wall clock)
  3. Circuit breaker (≥10 min idle) → force pause, skip bandit
  4. Build context → LinUCB select
  5. Pause only if AllowSleep and idle ≥ 60s; otherwise keep running
  6. Stash decision for next poll

Checkpoint: ``~/.local/share/docked-browser/linucb.json``
Per-profile last-used: ``~/.local/share/docked-browser/predict-sleep-state.json``
"""

from __future__ import annotations

import fcntl
import json
import math
import os
import re
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# --- Constants (match ambient-sleep) -----------------------------------------
FEATURE_DIM = 16
FEATURE_LAYOUT = 2  # v2: container CPU is not an activity signal
POLL_INTERVAL_S = 60
MIN_AI_PAUSE_IDLE_MS = 60_000
IDLE_TRIP_MS = 5 * 60 * 1000  # unused profile hard-pause (was 10m)
FORCED_WAKE_WINDOW_S = 10 * 60
WARNING_SECS = 20
CANCEL_GRACE_SECS = 15 * 60  # after click-cancel, skip auto-pause for this long
MYT_OFFSET_SECS = 8 * 3600
ALPHA = 0.5
# docker top PID cache — warning poll checks focus every 400ms
_PID_CACHE_TTL_S = 2.0

PROFILE_RE = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")
STATE_DIR = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "docked-browser"
MODEL_PATH = STATE_DIR / "linucb.json"
SLEEP_STATE_PATH = STATE_DIR / "predict-sleep-state.json"
LOCK_PATH = STATE_DIR / "predict-sleep.lock"
ACTIVE_FILE = STATE_DIR / "active-profile"
CLI = Path(__file__).resolve().parent.parent / "bin" / "docked-browser"
WM_CLASS = "docked-browser"

ACTION_KEEP = "KeepRunning"
ACTION_PAUSE = "Pause"


# --- Small math helpers ------------------------------------------------------
def _mat_vec(dim: int, m: list[float], x: list[float]) -> list[float]:
    out = [0.0] * dim
    for i in range(dim):
        s = 0.0
        row = i * dim
        for j in range(dim):
            s += m[row + j] * x[j]
        out[i] = s
    return out


def _dot(a: list[float], b: list[float]) -> float:
    return sum(u * v for u, v in zip(a, b))


def _sherman_morrison(dim: int, a_inv: list[float], x: list[float]) -> bool:
    ax = _mat_vec(dim, a_inv, x)
    denom = 1.0 + _dot(x, ax)
    if abs(denom) < 1e-12:
        return False
    for i in range(dim):
        for j in range(dim):
            a_inv[i * dim + j] -= ax[i] * ax[j] / denom
    return True


def _invert(dim: int, a: list[float]) -> list[float] | None:
    wide = 2 * dim
    m = [0.0] * (dim * wide)
    for i in range(dim):
        for j in range(dim):
            m[i * wide + j] = a[i * dim + j]
        m[i * wide + dim + i] = 1.0
    for col in range(dim):
        pivot = col
        best = abs(m[col * wide + col])
        for row in range(col + 1, dim):
            v = abs(m[row * wide + col])
            if v > best:
                best = v
                pivot = row
        if best < 1e-12:
            return None
        if pivot != col:
            for j in range(wide):
                m[col * wide + j], m[pivot * wide + j] = m[pivot * wide + j], m[col * wide + j]
        diag = m[col * wide + col]
        for j in range(wide):
            m[col * wide + j] /= diag
        for row in range(dim):
            if row == col:
                continue
            factor = m[row * wide + col]
            if factor == 0.0:
                continue
            for j in range(wide):
                m[row * wide + j] -= factor * m[col * wide + j]
    inv = [0.0] * (dim * dim)
    for i in range(dim):
        for j in range(dim):
            inv[i * dim + j] = m[i * wide + dim + j]
    return inv


def _fnv1a(text: str) -> int:
    h = 0xCBF29CE484222325
    for b in text.encode("utf-8"):
        h ^= b
        h = (h * 0x0100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _identity_arm(dim: int) -> tuple[list[float], list[float], list[float]]:
    a = [0.0] * (dim * dim)
    a_inv = [0.0] * (dim * dim)
    for i in range(dim):
        a[i * dim + i] = 1.0
        a_inv[i * dim + i] = 1.0
    return a, [0.0] * dim, a_inv


def malaysia_hour() -> float:
    now = time.time() + MYT_OFFSET_SECS
    day = now % 86400
    return day / 3600.0


# --- Context / rewards -------------------------------------------------------
@dataclass
class Context:
    profile: str
    is_active: bool
    chrome_focused: bool
    cpu_busy: bool
    hour_sin: float
    hour_cos: float
    recent_forced_wakes: int
    idle_ms: int

    def features(self) -> list[float]:
        idle_norm = min(1.0, max(0.0, self.idle_ms / 600_000.0))
        active = 1.0 if self.is_active else 0.0
        focused = 1.0 if self.chrome_focused else 0.0
        x = [0.0] * FEATURE_DIM
        x[0] = 1.0
        x[1] = active
        x[2] = focused
        x[3] = active * focused
        x[4] = 1.0 if self.cpu_busy else 0.0
        x[6] = self.hour_sin
        x[7] = self.hour_cos
        x[8] = min(1.0, self.recent_forced_wakes / 5.0)
        x[9] = idle_norm
        x[10] = 1.0 - active
        x[11] = active * idle_norm
        # 14–15: signed FNV hash of profile name
        if self.profile:
            h = _fnv1a(self.profile.lower())
            bucket = h % 2
            sign = 1.0 if ((h >> 32) & 1) == 0 else -1.0
            x[14 + bucket] = sign
        return x


def reward_from_idle(action: str, idle_after_ms: int, wall_elapsed_ms: int) -> float:
    forced_wake_ms = 15_000
    still_active_ms = 30_000
    gone_ms = 5 * 60 * 1000
    if action == ACTION_PAUSE:
        absence = max(idle_after_ms, wall_elapsed_ms)
        if absence >= gone_ms:
            return 0.5
        if idle_after_ms < forced_wake_ms:
            return -1.0
        return 0.0
    # KeepRunning
    if idle_after_ms < still_active_ms:
        return 1.0
    if idle_after_ms >= gone_ms:
        return -0.05
    return 0.2


def is_forced_wake(action: str, idle_after_ms: int, wall_elapsed_ms: int) -> bool:
    return (
        action == ACTION_PAUSE
        and idle_after_ms < 15_000
        and wall_elapsed_ms < 5 * 60 * 1000
    )


def heuristic_action(ctx: Context) -> str:
    # The focused Chrome window stays up. Container CPU is not activity.
    if ctx.chrome_focused:
        return ACTION_KEEP
    if ctx.idle_ms < MIN_AI_PAUSE_IDLE_MS:
        return ACTION_KEEP
    return ACTION_PAUSE


# --- LinUCB ------------------------------------------------------------------
class Arm:
    __slots__ = ("a", "b", "a_inv")

    def __init__(self, dim: int, a: list[float] | None = None, b: list[float] | None = None):
        if a is None or b is None:
            self.a, self.b, self.a_inv = _identity_arm(dim)
        else:
            self.a = list(a)
            self.b = list(b)
            inv = _invert(dim, self.a)
            self.a_inv = inv if inv is not None else _identity_arm(dim)[2]

    def observe(self, dim: int, x: list[float], reward: float) -> None:
        for i in range(dim):
            for j in range(dim):
                self.a[i * dim + j] += x[i] * x[j]
            self.b[i] += reward * x[i]
        if not _sherman_morrison(dim, self.a_inv, x):
            inv = _invert(dim, self.a)
            self.a_inv = inv if inv is not None else _identity_arm(dim)[2]

    def ucb(self, dim: int, x: list[float], alpha: float) -> float:
        theta = _mat_vec(dim, self.a_inv, self.b)
        exploit = _dot(theta, x)
        ax = _mat_vec(dim, self.a_inv, x)
        explore = math.sqrt(max(0.0, _dot(x, ax)))
        return exploit + alpha * explore


class LinUcb:
    def __init__(self, alpha: float = ALPHA, dim: int = FEATURE_DIM, path: Path = MODEL_PATH):
        self.alpha = alpha
        self.dim = dim
        self.path = path
        self.arms = [Arm(dim), Arm(dim)]
        self.decisions = 0
        self.updates = 0
        self.last_reward: float | None = None
        self.last_ucb: tuple[float, float] | None = None
        self._apply_heuristic_prior()

    def _apply_heuristic_prior(self) -> None:
        dim = self.dim
        # Active + focused → KeepRunning
        focused = [0.0] * dim
        focused[0] = 1.0
        focused[1] = 1.0
        focused[2] = 1.0
        focused[3] = 1.0
        self.arms[0].observe(dim, focused, 1.0)
        # Recent forced wakes → KeepRunning
        wakes = [0.0] * dim
        wakes[0] = 1.0
        wakes[8] = 1.0
        self.arms[0].observe(dim, wakes, 1.0)
        # Idle + not active → Pause
        idle = [0.0] * dim
        idle[0] = 1.0
        idle[9] = 0.6
        idle[10] = 1.0
        self.arms[1].observe(dim, idle, 0.6)

    @classmethod
    def load_or_default(cls, path: Path = MODEL_PATH) -> "LinUcb":
        try:
            return cls.load(path)
        except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError):
            return cls(path=path)

    @classmethod
    def load(cls, path: Path) -> "LinUcb":
        snap = json.loads(path.read_text(encoding="utf-8"))
        if snap.get("layout") != FEATURE_LAYOUT or snap.get("dim") != FEATURE_DIM:
            raise ValueError("linucb checkpoint layout/dim mismatch")
        policy = cls(alpha=float(snap.get("alpha", ALPHA)), dim=FEATURE_DIM, path=path)
        policy.arms = [
            Arm(FEATURE_DIM, snap["keep"]["a"], snap["keep"]["b"]),
            Arm(FEATURE_DIM, snap["pause"]["a"], snap["pause"]["b"]),
        ]
        policy.decisions = int(snap.get("decisions", 0))
        policy.updates = int(snap.get("updates", 0))
        # Loaded arms already include prior if saved after prior; skip re-prior
        return policy

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        snap = {
            "layout": FEATURE_LAYOUT,
            "alpha": self.alpha,
            "dim": self.dim,
            "decisions": self.decisions,
            "updates": self.updates,
            "keep": {"a": self.arms[0].a, "b": self.arms[0].b},
            "pause": {"a": self.arms[1].a, "b": self.arms[1].b},
        }
        tmp = self.path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(snap, indent=2) + "\n", encoding="utf-8")
        tmp.replace(self.path)

    def select(self, ctx: Context) -> str:
        self.decisions += 1
        x = ctx.features()
        stay = self.arms[0].ucb(self.dim, x, self.alpha)
        allow = self.arms[1].ucb(self.dim, x, self.alpha)
        self.last_ucb = (stay, allow)
        if abs(stay - allow) < 1e-9:
            return heuristic_action(ctx)
        return ACTION_KEEP if stay >= allow else ACTION_PAUSE

    def update(self, ctx: Context, action: str, reward: float) -> None:
        reward = max(-1.0, min(1.0, reward))
        x = ctx.features()
        idx = 0 if action == ACTION_KEEP else 1
        self.arms[idx].observe(self.dim, x, reward)
        self.updates += 1
        self.last_reward = reward


# --- Host sensors ------------------------------------------------------------
def _run(cmd: list[str], timeout: float = 3.0) -> str:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return ""
    return (r.stdout or "").strip() if r.returncode == 0 else ""


def system_idle_ms() -> int:
    # Mutter IdleMonitor (GNOME)
    out = _run(
        [
            "gdbus",
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.IdleMonitor",
            "--object-path",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "--method",
            "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ]
    )
    if out:
        # e.g. (uint64 12345,)
        m = re.search(r"(\d+)", out)
        if m:
            return int(m.group(1))
    out = _run(["xprintidle"])
    if out.isdigit():
        return int(out)
    return 0


_PID_CACHE: dict[str, tuple[float, set[int]]] = {}


def _parse_focused_payload(out: str) -> dict | None:
    """Pull the JSON object out of a gdbus string reply."""
    start = out.find("{")
    end = out.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        data = json.loads(out[start : end + 1])
    except json.JSONDecodeError:
        return None
    return data if isinstance(data, dict) else None


def _focused_window_from_shell() -> dict | None:
    """Focused window via the Focused Window D-Bus helper (GNOME / Wayland)."""
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
    return _parse_focused_payload(out) if out else None


def _focused_window_x11() -> tuple[int, str]:
    wid = _run(["xdotool", "getactivewindow"])
    if not wid.isdigit():
        return 0, ""
    cls = _run(["xprop", "-id", wid, "WM_CLASS"]).lower()
    pid_s = _run(["xprop", "-id", wid, "_NET_WM_PID"])
    match = re.search(r"(\d+)", pid_s)
    return (int(match.group(1)) if match else 0), cls


def _window_pid_and_class() -> tuple[int, str]:
    info = _focused_window_from_shell()
    if info:
        wm = str(info.get("wm_class") or info.get("wm_class_instance") or "").lower()
        try:
            pid = int(info.get("pid") or 0)
        except (TypeError, ValueError):
            pid = 0
        if info.get("focus") is False:
            return 0, wm
        if pid > 0:
            return pid, wm
    return _focused_window_x11()


def container_host_pids(container: str) -> set[int]:
    now = time.monotonic()
    cached = _PID_CACHE.get(container)
    if cached is not None and now - cached[0] < _PID_CACHE_TTL_S:
        return cached[1]
    out = _run(["docker", "top", container, "-eo", "pid"], timeout=8.0)
    pids = {int(line) for line in out.splitlines() if line.strip().isdigit()}
    _PID_CACHE[container] = (now, pids)
    return pids


def focused_docked_profile(running: dict[str, dict]) -> str:
    """Profile that owns the focused Chrome window, or '' if something else is focused.

    Matched by the window PID against ``docker top`` of chrome-* containers.
    Container CPU is ignored — a quiet page the user is reading still counts.
    """
    pid, wm = _window_pid_and_class()
    if pid <= 0:
        return ""
    if wm and WM_CLASS not in wm and "chrome" not in wm and "chromium" not in wm:
        return ""
    for profile, meta in running.items():
        name = str(meta.get("name") or f"chrome-{profile}")
        if pid in container_host_pids(name):
            return profile
    return ""


def get_active_profile() -> str:
    try:
        name = ACTIVE_FILE.read_text(encoding="utf-8").strip()
    except OSError:
        return ""
    return name if PROFILE_RE.match(name) else ""


def docker_running_profiles() -> dict[str, dict]:
    """profile → {name} for running (not paused) chrome-* containers."""
    try:
        result = subprocess.run(
            [
                "docker",
                "ps",
                "--filter",
                "name=chrome-",
                "--filter",
                "status=running",
                "--format",
                "{{.Names}}",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {}
    names = [ln.strip() for ln in result.stdout.splitlines() if ln.strip().startswith("chrome-")]
    out: dict[str, dict] = {}
    for name in names:
        profile = name[len("chrome-") :]
        if not PROFILE_RE.match(profile):
            continue
        out[profile] = {"name": name}
    return out


def pause_profile(profile: str) -> bool:
    if not CLI.is_file() or not PROFILE_RE.match(profile):
        return False
    try:
        r = subprocess.run(
            [str(CLI), "pause", profile],
            capture_output=True,
            text=True,
            timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return r.returncode == 0


def _profile_seems_used(profile: str) -> bool:
    """Cancel the pause warning when this profile's window is the focused one."""
    if not PROFILE_RE.match(profile):
        return False
    return focused_docked_profile(docker_running_profiles()) == profile


def warn_before_pause(profile: str) -> bool:
    """Show a notification for ``WARNING_SECS``.

    Returns True if the user cancelled (clicked the notification or its
    Keep awake action, dismissed it, or focused the profile).

    GNOME Shell hides the banner after a few seconds and, for a *transient*
    notification, closes it as expired (reason 1). That used to be treated as
    "not cancelled", so a click lost the race and the profile was paused.
    The notification stays in the list until the user clicks or our timer
    fires. A shell close reason alone never counts as permission to pause.
    """
    if not PROFILE_RE.match(profile):
        return False
    try:
        import gi

        gi.require_version("Gio", "2.0")
        gi.require_version("GLib", "2.0")
        from gi.repository import Gio, GLib
    except Exception as exc:  # noqa: BLE001 — PyGObject optional at import time
        _log(f"gi Notifications unavailable ({exc}) — timed wait only")
        return _warn_before_pause_wait_only(profile)

    # Fresh connection. Gio.bus_get_sync() caches one session bus for the
    # process; once that socket closes, every later pause warning crashes
    # and the profile never pauses.
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    flags = (
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    )
    try:
        if address:
            bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
        else:
            bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        proxy = Gio.DBusProxy.new_sync(
            bus,
            Gio.DBusProxyFlags.NONE,
            None,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            None,
        )
    except Exception as exc:  # noqa: BLE001
        _log(f"Notifications bus unavailable ({exc}) — timed wait only")
        return _warn_before_pause_wait_only(profile)

    state: dict[str, Any] = {"nid": 0, "cancelled": False, "done": False}
    loop = GLib.MainLoop()

    def _finish(cancelled: bool) -> None:
        if state["done"]:
            return
        state["done"] = True
        state["cancelled"] = cancelled
        loop.quit()

    def _same(nid: object) -> bool:
        try:
            return int(nid) == int(state["nid"])
        except (TypeError, ValueError):
            return False

    def on_signal(_conn, _sender, _path, _iface, signal, params, *_user):
        # Body click: ActivationToken then ActionInvoked("default"), then
        # NotificationClosed reason 2. The button uses action id "keep".
        # Do not treat reason 1/3/4 as "go ahead and pause" — GNOME emits
        # those when the banner hides or when we call CloseNotification.
        if signal in ("ActionInvoked", "ActivationToken"):
            nid, detail = params.unpack()
            if _same(nid):
                _log(f"{profile}: notification {signal} {detail!r}")
                _finish(True)
        elif signal == "NotificationClosed":
            nid, reason = params.unpack()
            if not _same(nid):
                return
            # 1=expired 2=dismissed 3=CloseNotification 4=undefined
            if reason == 2:
                _log(f"{profile}: notification dismissed")
                _finish(True)

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

    def poll_use() -> bool:
        if _profile_seems_used(profile):
            _finish(True)
            return False
        return True

    try:
        nid = proxy.call_sync(
            "Notify",
            GLib.Variant(
                "(susssasa{sv}i)",
                (
                    "Docked Browser",
                    0,
                    "web-browser",
                    f"Pausing “{profile}” in {WARNING_SECS}s",
                    "Click this notification to keep it awake.",
                    ["default", "Keep awake", "keep", "Keep awake"],
                    {
                        "suppress-sound": GLib.Variant("b", True),
                        "urgency": GLib.Variant("y", 1),
                    },
                    WARNING_SECS * 1000,
                ),
            ),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        ).unpack()[0]
        state["nid"] = int(nid)
        GLib.timeout_add(400, poll_use)
        GLib.timeout_add_seconds(WARNING_SECS + 1, lambda: (_finish(False), False)[1])
        loop.run()
    except Exception as exc:  # noqa: BLE001
        _log(f"Notify failed ({exc}) — timed wait only")
        return _warn_before_pause_wait_only(profile)
    finally:
        # Drop the match before CloseNotification. That close emits reason 3,
        # and call_sync pumps the main loop, which would otherwise see it.
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
                    -1,
                    None,
                )
        except Exception:  # noqa: BLE001
            pass
        try:
            bus.close_sync(None)
        except Exception:  # noqa: BLE001
            pass

    if not state["cancelled"]:
        _log(f"{profile}: warning timed out")
    return bool(state["cancelled"])


def _warn_before_pause_wait_only(profile: str) -> bool:
    """Countdown without a reliable click path (gi/notify missing)."""
    deadline = time.time() + WARNING_SECS
    while time.time() < deadline:
        if _profile_seems_used(profile):
            return True
        time.sleep(0.4)
    return False


def maybe_pause_with_warning(
    profile: str,
    *,
    bandit: LinUcb,
    ctx: Context,
    st: "ProfileSleepState",
    reason: str,
) -> tuple[bool, str]:
    """Warn, then pause unless the user clicks Keep awake.

    Returns (paused, log_line).
    """
    now = time.time()
    if st.cancel_grace_until and now < st.cancel_grace_until:
        left = int(st.cancel_grace_until - now)
        line = f"{profile}: skip pause (cancel grace {left}s left) reason={reason}"
        return False, line

    _log(f"{profile}: warning ({WARNING_SECS}s) reason={reason}")
    try:
        cancelled = warn_before_pause(profile)
    except Exception as exc:  # noqa: BLE001 — a broken notification must not skip pause
        _log(f"{profile}: warning failed ({exc}) — pausing anyway")
        cancelled = False
    if cancelled:
        mark_used(profile)
        st.last_used_at = time.time()
        st.auto_paused_at = None
        st.cancel_grace_until = time.time() + CANCEL_GRACE_SECS
        # Same as ambient-sleep: Cancel on warning → AllowSleep −1.0
        bandit.update(ctx, ACTION_PAUSE, -1.0)
        line = (
            f"{profile}: warning cancelled → Pause −1.0 "
            f"(grace {CANCEL_GRACE_SECS // 60}m)"
        )
        return False, line
    if pause_profile(profile):
        st.auto_paused_at = time.time()
        st.cancel_grace_until = None
        line = f"{profile}: {reason} → Pause (after warning)"
        return True, line
    return False, f"{profile}: {reason} pause-failed"


# --- Persisted per-profile idle / pending ------------------------------------
@dataclass
class ProfileSleepState:
    last_used_at: float = field(default_factory=time.time)
    last_wall_input_at: float = field(default_factory=time.time)
    auto_paused_at: float | None = None
    pending: dict[str, Any] | None = None  # ctx snapshot + action + decided_at + idle_ms
    recent_forced_wakes: list[float] = field(default_factory=list)
    cancel_grace_until: float | None = None


@dataclass
class SleepStore:
    profiles: dict[str, ProfileSleepState] = field(default_factory=dict)
    enabled: bool = True

    def get(self, name: str) -> ProfileSleepState:
        if name not in self.profiles:
            self.profiles[name] = ProfileSleepState()
        return self.profiles[name]

    @classmethod
    def load(cls) -> "SleepStore":
        store = cls()
        try:
            raw = json.loads(SLEEP_STATE_PATH.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return store
        store.enabled = bool(raw.get("enabled", True))
        for name, pdata in (raw.get("profiles") or {}).items():
            if not PROFILE_RE.match(name) or not isinstance(pdata, dict):
                continue
            st = ProfileSleepState(
                last_used_at=float(pdata.get("last_used_at", time.time())),
                last_wall_input_at=float(pdata.get("last_wall_input_at", time.time())),
                auto_paused_at=pdata.get("auto_paused_at"),
                pending=pdata.get("pending"),
                recent_forced_wakes=list(pdata.get("recent_forced_wakes") or []),
                cancel_grace_until=pdata.get("cancel_grace_until"),
            )
            if st.auto_paused_at is not None:
                st.auto_paused_at = float(st.auto_paused_at)
            if st.cancel_grace_until is not None:
                st.cancel_grace_until = float(st.cancel_grace_until)
            store.profiles[name] = st
        return store

    def save(self) -> None:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        payload = {
            "enabled": self.enabled,
            "profiles": {
                name: {
                    "last_used_at": st.last_used_at,
                    "last_wall_input_at": st.last_wall_input_at,
                    "auto_paused_at": st.auto_paused_at,
                    "pending": st.pending,
                    "recent_forced_wakes": st.recent_forced_wakes[-32:],
                    "cancel_grace_until": st.cancel_grace_until,
                }
                for name, st in self.profiles.items()
            },
        }
        tmp = SLEEP_STATE_PATH.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        tmp.replace(SLEEP_STATE_PATH)


def mark_used(profile: str) -> None:
    """Call when the user opens / resumes / focuses a profile (exact name)."""
    if not PROFILE_RE.match(profile):
        return
    store = SleepStore.load()
    st = store.get(profile)
    now = time.time()
    # Forced-wake credit if they woke an auto-paused profile quickly
    if st.auto_paused_at is not None:
        wall_ms = int((now - st.auto_paused_at) * 1000)
        if wall_ms < 5 * 60 * 1000:
            st.recent_forced_wakes.append(now)
            # Immediate negative reward on the pending Pause decision if still stored
            if st.pending and st.pending.get("action") == ACTION_PAUSE:
                bandit = LinUcb.load_or_default()
                ctx = _ctx_from_pending(st.pending, profile)
                bandit.update(ctx, ACTION_PAUSE, -1.0)
                try:
                    bandit.save()
                except OSError:
                    pass
                st.pending = None
        st.auto_paused_at = None
    st.last_used_at = now
    st.last_wall_input_at = now
    # Drop stale wakes
    cutoff = now - FORCED_WAKE_WINDOW_S
    st.recent_forced_wakes = [t for t in st.recent_forced_wakes if t >= cutoff]
    store.save()


def _ctx_from_pending(pending: dict[str, Any], profile: str) -> Context:
    hour = malaysia_hour()
    rad = hour * (math.tau / 24.0)
    return Context(
        profile=profile,
        is_active=bool(pending.get("is_active", False)),
        chrome_focused=bool(pending.get("chrome_focused", False)),
        cpu_busy=bool(pending.get("cpu_busy", False)),
        hour_sin=float(pending.get("hour_sin", math.sin(rad))),
        hour_cos=float(pending.get("hour_cos", math.cos(rad))),
        recent_forced_wakes=int(pending.get("recent_forced_wakes", 0)),
        idle_ms=int(pending.get("idle_ms", 0)),
    )


def fused_idle_ms(st: ProfileSleepState, os_idle: int, in_use: bool) -> int:
    """Fuse OS keyboard/mouse idle with wall time since this profile was frontmost.

    ``in_use`` means this profile's window is focused. Host input during the
    poll window counts as activity; container CPU does not.
    """
    now = time.time()
    if in_use and os_idle < POLL_INTERVAL_S * 1000:
        st.last_used_at = now - (os_idle / 1000.0)
        st.last_wall_input_at = st.last_used_at
        return os_idle
    if in_use:
        # Window still focused, but the keyboard has been quiet longer than a poll.
        return os_idle
    # Another window is frontmost: clock since the last time this profile was used.
    return max(0, int((now - st.last_used_at) * 1000))


# --- Tick --------------------------------------------------------------------
_log_lines: list[str] = []
_log_lock = threading.Lock()


def recent_log(limit: int = 40) -> list[str]:
    with _log_lock:
        return list(_log_lines[-limit:])


def _log(line: str) -> None:
    stamp = time.strftime("%H:%M:%S", time.gmtime(time.time() + MYT_OFFSET_SECS))
    msg = f"[{stamp}] {line}"
    with _log_lock:
        _log_lines.append(msg)
        if len(_log_lines) > 200:
            del _log_lines[:-100]
    print(f"[predict-sleep] {msg}", flush=True)


def status_payload() -> dict:
    store = SleepStore.load()
    bandit = LinUcb.load_or_default()
    profiles = {}
    now = time.time()
    for name, st in store.profiles.items():
        profiles[name] = {
            "idle_ms": max(0, int((now - st.last_used_at) * 1000)),
            "auto_paused_at": st.auto_paused_at,
            "pending_action": (st.pending or {}).get("action"),
            "forced_wakes_10m": len(
                [t for t in st.recent_forced_wakes if t >= now - FORCED_WAKE_WINDOW_S]
            ),
        }
    return {
        "enabled": store.enabled,
        "updates": bandit.updates,
        "decisions": bandit.decisions,
        "last_reward": bandit.last_reward,
        "model": str(MODEL_PATH),
        "profiles": profiles,
        "log": recent_log(20),
    }


def set_enabled(enabled: bool) -> dict:
    store = SleepStore.load()
    store.enabled = bool(enabled)
    store.save()
    return status_payload()


def tick(bandit: LinUcb | None = None, store: SleepStore | None = None) -> list[str]:
    """One poll cycle. Returns human-readable action lines."""
    bandit = bandit or LinUcb.load_or_default()
    store = store or SleepStore.load()
    lines: list[str] = []
    if not store.enabled:
        return lines

    running = docker_running_profiles()
    if not running:
        store.save()
        return lines

    os_idle = system_idle_ms()
    focused_name = focused_docked_profile(running)
    active = get_active_profile()
    now = time.time()
    changed = False

    for profile, _meta in running.items():
        st = store.get(profile)
        # Prune wakes
        cutoff = now - FORCED_WAKE_WINDOW_S
        st.recent_forced_wakes = [t for t in st.recent_forced_wakes if t >= cutoff]
        wakes = len(st.recent_forced_wakes)

        is_active = profile == active
        # In use only when this profile's window is focused. A quiet page
        # (near-zero CPU) still counts; background CPU does not.
        chrome_here = focused_name == profile
        in_use = chrome_here

        idle_ms = fused_idle_ms(st, os_idle, in_use)

        # Credit previous decision
        if st.pending:
            wall_ms = int((now - float(st.pending.get("decided_at", now))) * 1000)
            prev_action = str(st.pending.get("action", ACTION_KEEP))
            prev_ctx = _ctx_from_pending(st.pending, profile)
            reward = reward_from_idle(prev_action, idle_ms, wall_ms)
            bandit.update(prev_ctx, prev_action, reward)
            if is_forced_wake(prev_action, idle_ms, wall_ms):
                st.recent_forced_wakes.append(now)
                wakes = len(st.recent_forced_wakes)
            st.pending = None
            changed = True

        hour = malaysia_hour()
        rad = hour * (math.tau / 24.0)
        ctx = Context(
            profile=profile,
            is_active=is_active,
            chrome_focused=chrome_here,
            cpu_busy=False,
            hour_sin=math.sin(rad),
            hour_cos=math.cos(rad),
            recent_forced_wakes=wakes,
            idle_ms=idle_ms,
        )

        # Circuit breaker — force pause (still warn once), skip bandit
        if idle_ms >= IDLE_TRIP_MS and not in_use:
            paused, line = maybe_pause_with_warning(
                profile, bandit=bandit, ctx=ctx, st=st, reason=f"BREAKER idle_ms={idle_ms}"
            )
            lines.append(line)
            _log(line)
            st.pending = None
            changed = True
            continue

        action = bandit.select(ctx)
        applied = ACTION_KEEP

        if action == ACTION_PAUSE and idle_ms >= MIN_AI_PAUSE_IDLE_MS and not in_use:
            paused, line = maybe_pause_with_warning(
                profile,
                bandit=bandit,
                ctx=ctx,
                st=st,
                reason=(
                    f"action=Pause idle_ms={idle_ms} "
                    f"active={int(is_active)} focused={focused_name or '-'} "
                    f"n={bandit.updates}"
                ),
            )
            lines.append(line)
            _log(line)
            changed = True
            if paused:
                applied = ACTION_PAUSE
            else:
                # Cancel already credited Pause −1.0 — do not pending-credit again.
                st.pending = None
                continue
        elif action == ACTION_PAUSE:
            applied = f"{ACTION_KEEP}(idle-floor)"
        else:
            applied = ACTION_KEEP

        st.pending = {
            "action": action,
            "decided_at": now,
            "idle_ms": idle_ms,
            "is_active": is_active,
            "chrome_focused": chrome_here,
            "cpu_busy": False,
            "hour_sin": ctx.hour_sin,
            "hour_cos": ctx.hour_cos,
            "recent_forced_wakes": wakes,
        }
        if applied.startswith(ACTION_KEEP) and action == ACTION_KEEP:
            ucb = bandit.last_ucb or (0.0, 0.0)
            _log(
                f"{profile}: KeepRunning idle_ms={idle_ms} "
                f"ucb=({ucb[0]:.3f},{ucb[1]:.3f}) n={bandit.updates}"
            )

    if changed or True:
        try:
            bandit.save()
        except OSError as exc:
            _log(f"linucb save failed: {exc}")
        store.save()
    return lines


# --- Daemon ------------------------------------------------------------------
class PredictSleepDaemon:
    """Background poller with exclusive lock (GUI server or tray)."""

    def __init__(self) -> None:
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._lock_fd: int | None = None

    def try_start(self) -> bool:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        try:
            fd = os.open(str(LOCK_PATH), os.O_CREAT | os.O_RDWR, 0o644)
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            os.write(fd, f"{os.getpid()}\n".encode())
            self._lock_fd = fd
        except OSError:
            return False
        self._thread = threading.Thread(target=self._loop, name="predict-sleep", daemon=True)
        self._thread.start()
        _log("daemon started")
        return True

    def stop(self) -> None:
        self._stop.set()
        if self._lock_fd is not None:
            try:
                fcntl.flock(self._lock_fd, fcntl.LOCK_UN)
                os.close(self._lock_fd)
            except OSError:
                pass
            self._lock_fd = None

    def _loop(self) -> None:
        # Fresh model on first boot
        bandit = LinUcb.load_or_default()
        if bandit.updates == 0:
            try:
                bandit.save()
            except OSError:
                pass
        while not self._stop.wait(POLL_INTERVAL_S):
            try:
                tick(bandit)
            except Exception as exc:  # noqa: BLE001 — keep daemon alive
                _log(f"tick error: {exc}")


_daemon: PredictSleepDaemon | None = None


def ensure_daemon() -> bool:
    global _daemon
    if _daemon is not None:
        return True
    d = PredictSleepDaemon()
    if d.try_start():
        _daemon = d
        return True
    return False
