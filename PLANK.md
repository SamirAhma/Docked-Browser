# Dock / raise internals

Facts for humans and for LLMs changing Docked Browser’s dock or how Chrome attaches to the desktop. Behavior below is what `bin/docked-browser`, `gui/predict_sleep.py`, and `bin/focus-modal` do. Broader repo rules stay in [AGENTS.md](./AGENTS.md).

**Supported / tested:** Ubuntu **26.04** LTS, **GNOME on Wayland** (Shell 50). That is the product target.

**Not supported / not tested:** Plank, Cinnamon, X11, Ubuntu on Xorg. GNOME on Xorg black-screens on Shell 50 — do not switch the user there. Sections below that mention Plank or X11 describe leftover code only; do not expand or advertise them.

## Untested: Plank leftovers

`sync_plank_launchers` still exists in `bin/docked-browser`. It is **not** a supported product surface. If you must touch it, the interface is files Plank already watches:

`~/.config/plank/dock1/launchers/*.dockitem`

(or another `dockN` directory that `gsettings get net.launchpad.plank enabled-docks` lists). `sync_plank_launchers` in `bin/docked-browser` writes only this app’s pins. One write, `O_CREAT|O_EXCL`, so the directory watch sees a finished file:

```
[PlankDockItemPreferences]
Launcher=file:///home/<user>/.local/share/applications/docked-browser-<profile>.desktop
```

The filename is `docked-browser-<profile>.dockitem`. The `Launcher=` value is the absolute `file://` URI of that profile’s `.desktop`.

### Plank pin lifecycle (untested)

A pin exists only when `docker inspect` says `chrome-<profile>` is **running**. Paused, stopped, and missing containers have no dockitem. The Chrome window stays mapped across pause. `install_dock_entry` still calls `sync_plank_launchers` from lifecycle commands.

Plank with `pinned-only` false can turn a removed pin into a running-window icon while that window is mapped. The leftover code then uses skip-taskbar and Plank D-Bus item removal. None of this is a supported target. (GNOME wake-on-focus lives in `gui/wake_paused.py` and is separate.)

## Window class

These strings are the same value, `docked-browser-<profile>`:

- Chrome `--class` and `--name` in `cmd_run`
- `StartupWMClass=` in the profile `.desktop` (`write_profile_desktop`)
- the Dash launcher for that `.desktop`

Left-click `Exec` is `bin/docked-browser activate <profile>`.

When the Chrome window is still mapped, GNOME Dash often activates the existing window by `StartupWMClass` and does **not** re-run Exec. That includes `docker pause` (the window stays mapped). So a left-click on a paused profile’s Dash icon may only raise the frozen window.

`gui/wake_paused.py` closes that gap on the supported GNOME Wayland path: it polls `FocusedWindow.Get`, and when the focused class is `docked-browser-<profile>` and `chrome-<profile>` is paused, it runs `activate` for that name. Wake is edge-triggered on focus enter (debounce 2s) so a pause while that window is already focused does not immediately fight resume. The watcher is started from the GUI server, tray, predict-sleep daemon, and on `pause` / `pause-all`.

Right-click `Actions=` always lists **Pause** and **Resume** for that profile (GNOME Shell freezes `Actions=` from the first load, so the menu must not swap them by state). Profiles…, Resume all, and Close all are shared fleet items and appear only when valid. `install_dock_entry` rewrites every profile `.desktop` when fleet state changes, then runs `sync_paused_dash` (and still calls the untested Plank sync).

`--user-data-dir` is set as well. The named data dir plus `--class` / `--name` keeps each profile on its own launcher.

## GNOME Wayland (supported)

`cmd_run` launches with `WAYLAND_DISPLAY`, the Wayland socket mount, and `--ozone-platform=wayland`.

Raise the window by class `docked-browser-<profile>` (`_focus_profile_class`). The container PID is not the window id (`MetaWindow.get_pid()` does not match the container).

D-Bus `org.gnome.shell.extensions.FocusedWindow.ActivateClass` on the Focused Window helper (`focused-window-dbus@flexagoon.com`, patched in place). The running Shell loaded that JavaScript at login. A method added on disk is absent until the next login. The same helper also gets `SetDashHidden` (paused profiles leave the Dash when loaded) and `OpenWorkspace` (switch to a workspace the user already created). `Get` (stock) feeds predict-sleep and wake-on-focus. The unused tree `gnome/docked-browser-focus@bukit` is not on this path.

## Untested: X11 / Cinnamon leftovers

`session_wants_x11` and the ozone-x11 launch path still exist. They are **not** supported. Same for the xdotool/xprop focus fallback inside `gui/wake_paused.py` and `wmctrl` / `xdotool` raise. Do not document them as working targets.

## Predict sleep

`gui/predict_sleep.py` is this repo’s per-profile LinUCB daemon (also started by `bin/docked-predict-sleep`, the GUI, or the tray). It pauses Chrome profiles. It is not the OS sleep daemon (`sleep-bandit` / ambient-sleep).

- Poll every **60s** (`POLL_INTERVAL_S`). The loop waits one interval before the first tick.
- **In use** means that profile’s Chrome window is the focused one: focused-window PID is in `docker top` of `chrome-<profile>`. Container CPU is ignored. A focused window is not auto-paused.
- LinUCB may choose Pause only after fused idle is at least **60s** (`MIN_AI_PAUSE_IDLE_MS`) and the window is not in use.
- Circuit breaker: idle at least **5 minutes** (`IDLE_TRIP_MS`) and not in use forces pause and skips the bandit. The constant is what runs.
- Either path shows a **20s** notification first (`WARNING_SECS`). “Keep awake”, a click on the notification, or focusing that profile cancels the pause and sets a 15-minute grace (`CANCEL_GRACE_SECS`). During the warning, focus is rechecked every 400ms.
- The pause itself is `bin/docked-browser pause <profile>` (`pause_profile`). That command runs `docker pause` and refreshes dock entries. Open and Resume call `activate`, which also `mark_used` for that exact name.

## Focus modal

`bin/focus-modal` calls `list_profiles()` in `gui/control.py`. That list is every known profile: data dirs under `~/.config/docker-chrome-profiles/` plus any `chrome-*` container, labeled running, paused, or stopped. A row click runs `activate` for that name. Right-click Focus and Resume do the same; Pause and Close call `pause` and `stop`.

## Do not reintroduce

Details and the full hard-constraint list are in [AGENTS.md](./AGENTS.md). Short form for this area:

- Dimming a dock icon, or swapping `Icon=`, on pause
- Icons under `~/.local/share/icons/hicolor/`, or a hand-written `hicolor/index.theme`
- Host NVIDIA `.so` bind-mounts into the Debian Chrome image
- The claim that `docker pause` frees RAM or writes memory to disk (it freezes CPU; RAM stays)
- One shared `StartupWMClass` or one shared Dash icon for every profile
- Claims that Plank, Cinnamon, or X11 are supported
- `docker run` from `gui/server.py` (the GUI calls `bin/docked-browser`)
