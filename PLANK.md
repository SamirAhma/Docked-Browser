# Plank and the X11 session

Facts for humans and for LLMs changing Docked Browser’s dock or how Chrome attaches to the desktop. Behavior below is what `bin/docked-browser`, `gui/predict_sleep.py`, `gui/wake_paused.py`, and `bin/focus-modal` do. Broader repo rules stay in [AGENTS.md](./AGENTS.md).

Live desktop: **Ubuntu GNOME on Wayland**. Cinnamon on X11 is still a supported path in `bin/docked-browser`. GNOME on Xorg (Ubuntu on Xorg) does not run on GNOME Shell 50 — black screen, then back to the login greeter. Do not switch the user to Ubuntu on Xorg.

## Do not fork Plank

Plank is a dependency. The interface is files it already watches:

`~/.config/plank/dock1/launchers/*.dockitem`

(or another `dockN` directory that `gsettings get net.launchpad.plank enabled-docks` lists). `sync_plank_launchers` in `bin/docked-browser` writes only this app’s pins. One write, `O_CREAT|O_EXCL`, so the directory watch sees a finished file:

```
[PlankDockItemPreferences]
Launcher=file:///home/<user>/.local/share/applications/docked-browser-<profile>.desktop
```

The filename is `docked-browser-<profile>.dockitem`. The `Launcher=` value is the absolute `file://` URI of that profile’s `.desktop`.

## One icon while the container is running

A pin exists only when `docker inspect` says `chrome-<profile>` is **running**. Paused, stopped, and missing containers have no dockitem. The Chrome window stays mapped across pause.

The same sync runs from `pause`, `resume`, `activate`, `run`, `stop`, `delete`, `pause-all`, `resume-all`, `stop-all`, and `refresh-dock` (each calls `install_dock_entry`). Other files in `launchers/` — the user’s Chrome, files, terminal — are left in place. Icon files stay as they are: no dimming, no `Icon=` swap.

## Deleting the pin is not enough

Plank with `pinned-only` false (`net.launchpad.plank.dock.settings` … `pinned-only`) turns a removed pin into a running-window icon while that window is mapped. Pause does not unmap the window, so the icon would come back.

Inside `sync_plank_launchers`:

1. `sync_dock` deletes `docked-browser-<profile>.dockitem` when that profile is not running, and writes it back when it is.
2. `sync_profile_taskbar` / `_set_skip_taskbar` sends `_NET_WM_STATE_SKIP_TASKBAR` on windows whose `WM_CLASS` is `docked-browser-<profile>` for every profile that is not running. Running profiles get that hint removed so the pin can attach.
3. After a short wait, `drop_hidden_plank_items` asks Plank (D-Bus `net.launchpad.plank.Items`) to drop leftover persistent and transient items for those hidden `.desktop` URIs. If `pinned-only` is false and a transient icon is still there, it sets `pinned-only` true, then sets it false again, so Plank rebuilds its running-window list without that profile.

`ensure_plank_pins` adds a running profile whose pin file Plank did not load. Skip-taskbar and item removal run only after a successful `docker inspect`. A Docker outage must not wipe every profile icon.

Clicking the still-visible paused window is handled separately. `pause` and `pause-all` start `gui/wake_paused.py` on X11. A click on a window whose class is `docked-browser-<profile>`, while `chrome-<profile>` is paused, runs `bin/docked-browser activate <profile>`. Cinnamon’s click ping would otherwise report the frozen Chrome as not responding.

## Window class

These three strings are the same value, `docked-browser-<profile>`:

- Chrome `--class` and `--name` in `cmd_run`
- `StartupWMClass=` in the profile `.desktop` (`write_profile_desktop`)
- the Plank pin, which launches that `.desktop`

Left-click `Exec` is `bin/docked-browser activate <profile>`.

Right-click `Actions=` always lists **Pause** and **Resume** for that profile (GNOME Shell freezes `Actions=` from the first load, so the menu must not swap them by state). Profiles…, Resume all, and Close all are shared fleet items and appear only when valid. `install_dock_entry` rewrites every profile `.desktop` when fleet state changes, then runs `sync_paused_dash` and `sync_plank_launchers`.

`--user-data-dir` is set as well. An instance name of `google-chrome` makes Plank group the window onto the host Chrome icon. The named data dir plus `--class` / `--name` keeps this window on its own launcher.

## X11 / Cinnamon vs GNOME Wayland

`session_wants_x11` is true when `XDG_SESSION_TYPE` is `x11`, or when the desktop name contains Cinnamon, Xfce, or MATE.

On that path `ensure_session_env` clears `WAYLAND_DISPLAY`, defaults `DISPLAY` to `:0`, and sets `XAUTHORITY` from the existing runtime `gdm/Xauthority` or `~/.Xauthority` when those files exist. A leftover `wayland-0` socket in `XDG_RUNTIME_DIR` is a previous GNOME login. Pointing Chrome at it exits with “Failed to connect to Wayland display”.

`cmd_run` then launches with:

- `-e DISPLAY`
- the `XAUTHORITY` file bind-mounted into the container
- `-v /tmp/.X11-unix:/tmp/.X11-unix:ro`
- `--ozone-platform=x11`

GNOME Wayland keeps its own path: `WAYLAND_DISPLAY`, the Wayland socket mount, and `--ozone-platform=wayland`.

`activate` / `unpause_profile` check the container command (`container_display_mismatch`). A profile last started with `--ozone-platform=wayland` is opened again with `run` on the current display so unpause is not asked to map a Wayland window on Cinnamon.

Raise the window by class `docked-browser-<profile>` (`_focus_profile_class`). The container PID is not the window id (on Wayland, `MetaWindow.get_pid()` does not match the container).

- X11: `wmctrl -lx` then `wmctrl -i -a`; if that misses, `xdotool search --class` / `--classname`; if that misses, a `_NET_ACTIVE_WINDOW` ClientMessage. Match the class string either way.
- GNOME Wayland: D-Bus `org.gnome.shell.extensions.FocusedWindow.ActivateClass` on the Focused Window helper (`focused-window-dbus@flexagoon.com`, patched in place). The running Shell loaded that JavaScript at login. A method added on disk is absent until the next login. The same helper also gets `SetDashHidden` (paused profiles leave the Dash) and `OpenWorkspace` (switch to a workspace the user already created). The unused tree `gnome/docked-browser-focus@bukit` is not on this path.

## Predict sleep

`gui/predict_sleep.py` is this repo’s per-profile LinUCB daemon (also started by `bin/docked-predict-sleep`, the GUI, or the tray). It pauses Chrome profiles. It is not the OS sleep daemon (`sleep-bandit` / ambient-sleep).

- Poll every **60s** (`POLL_INTERVAL_S`). The loop waits one interval before the first tick.
- **In use** means that profile’s Chrome window is the focused one: focused-window PID is in `docker top` of `chrome-<profile>`. Container CPU is ignored. A focused window is not auto-paused.
- LinUCB may choose Pause only after fused idle is at least **60s** (`MIN_AI_PAUSE_IDLE_MS`) and the window is not in use.
- Circuit breaker: idle at least **5 minutes** (`IDLE_TRIP_MS`) and not in use forces pause and skips the bandit. The constant is what runs.
- Either path shows a **20s** notification first (`WARNING_SECS`). “Keep awake”, a click on the notification, or focusing that profile cancels the pause and sets a 15-minute grace (`CANCEL_GRACE_SECS`). During the warning, focus is rechecked every 400ms.
- The pause itself is `bin/docked-browser pause <profile>` (`pause_profile`). That command runs `docker pause` and then the same Plank sync as a manual pause. Open and Resume call `activate`, which also `mark_used` for that exact name.

## Focus modal

`bin/focus-modal` calls `list_profiles()` in `gui/control.py`. That list is every known profile: data dirs under `~/.config/docker-chrome-profiles/` plus any `chrome-*` container, labeled running, paused, or stopped. A row click runs `activate` for that name. Right-click Focus and Resume do the same; Pause and Close call `pause` and `stop`.

## Do not reintroduce

Details and the full hard-constraint list are in [AGENTS.md](./AGENTS.md). Short form for this area:

- Dimming a dock icon, or swapping `Icon=`, on pause
- Icons under `~/.local/share/icons/hicolor/`, or a hand-written `hicolor/index.theme`
- Host NVIDIA `.so` bind-mounts into the Debian Chrome image
- The claim that `docker pause` frees RAM or writes memory to disk (it freezes CPU; RAM stays)
- One shared `StartupWMClass` or one shared Plank/Dash icon for every profile
- `docker run` from `gui/server.py` (the GUI calls `bin/docked-browser`)
