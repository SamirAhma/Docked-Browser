# AGENTS.md — Docked Browser

Instructions for AI assistants (and humans) editing this repo.

Plank pins, skip-taskbar, window class, and the X11 versus Wayland launch and raise path are in **[PLANK.md](./PLANK.md)**. Read that before changing the dock or how Chrome attaches to the session. Predict-sleep timings in that file follow the constants in `gui/predict_sleep.py`.

## Architecture

```
profile dock icon (Plank on Cinnamon, Dash on GNOME) ──click──► bin/docked-browser activate <name>
web UI / tray / focus-modal ──────────► bin/docked-browser ──► docker Chrome
```

- Keep stdlib `http.server` for the full UI (no Flask).
- Lifecycle goes through `bin/docked-browser`. Never call `docker run` from `gui/server.py`.
- **One dock icon per profile** (`docked-browser-<name>`). Left-click activates that profile.
- Right-click **Pause** and **Resume** both stay on every profile menu and target that profile. GNOME Shell 50 freezes `Actions=` from the first load, so the menu must not rely on swapping Pause for Resume by container state. Profiles…, Resume all, and Close all stay shared and appear only when valid.
- **Plank** (Cinnamon): one `.dockitem` per profile in `~/.config/plank/dock1/launchers/` (or another enabled `dockN`), only while `chrome-<name>` is running. Hide it when paused or stopped; put it back when running. The window stays on pause. A click on that window runs `activate` for that profile (`gui/wake_paused.py`): Cinnamon pings on click, and a paused Chrome cannot answer, so the shell otherwise shows “is not responding”. Never delete other Plank launchers.
- The live session is Ubuntu GNOME on Wayland. Raise by class with `ActivateClass`. A paused profile leaves the Dash via `SetDashHidden` (same helper). The user creates workspaces. `~/.config/docked-browser/workspaces.json` lists profile names under each workspace number they share (`"1"` is the first workspace). Names match exactly (case-sensitive). Several profiles may share one number. Select (`activate`) and resume, including the focus from `run`, open that workspace when the name is listed. A missing file, an empty file, or a name that is not listed only raises the window. If the number is past the workspaces that exist, the script prints that the workspace does not exist and still raises the window. `resume-all` does not switch workspaces. The app does not create workspaces and does not change Mutter settings. `ActivateClass`, `SetDashHidden`, and `OpenWorkspace` are patched into the installed Focused Window helper (`focused-window-dbus@flexagoon.com`). The running Shell does not load a new method until the next login; until then raise still works when `ActivateClass` is already loaded, Dash hide and workspace switch stay no-ops. Cinnamon on X11 stays supported: ignore a leftover `wayland-0` socket and launch with `DISPLAY`, `XAUTHORITY`, `/tmp/.X11-unix`, and `--ozone-platform=x11`, then raise with `wmctrl` or `xdotool`. Do not switch the user to Ubuntu on Xorg. Activate reopens a Wayland-started profile on the current display.
- The tree `gnome/docked-browser-focus@bukit` is unused. Live focus / Dash hide / workspace switch go through the patched Flexagoon helper above.

## Naming

| Concept | Pattern |
|---------|---------|
| Profile | `^[a-zA-Z0-9_-]{1,64}$` |
| Data dir | `~/.config/docker-chrome-profiles/<name>/` |
| Container | `chrome-<name>` |
| WM class / desktop | `docked-browser-<name>` |
| Profile dock icon | `~/.local/share/docked-browser/icons/docked-browser-<name>.png` |
| Brand glyph (web / tray / modal) | `~/.local/share/docked-browser/icons/docked-browser.png` |

`--class` / `--name` must match that profile’s `StartupWMClass` (`docked-browser-<name>`).

## Hard constraints

1. **Do not** try to dim dock icons on pause (GNOME caches window icons).
2. **Do not** put absolute icons under `hicolor` or invent `hicolor/index.theme`.
3. **No host NVIDIA `.so` bind-mounts** into the container.
4. **Pause does not free RAM.**
5. The focus modal lists every known profile (running, paused, stopped). A click runs `activate` for that exact name. Profiles… opens that picker. A profile’s dock click also runs `activate`.
6. Dock right-click `Actions=` are rewritten when fleet state changes. Pause and Resume always stay listed for that profile. Profiles…, Resume all, and Close all appear only when valid.
7. Each profile keeps its own Dash / Plank icon. Do not merge windows back onto one shared `docked-browser` class.
8. On GNOME, a **paused** profile leaves the Dash until resume (`SetDashHidden`). On Plank, the icon is shown only while the container is running (hidden when paused or stopped). The window stays on pause; do not dim or swap its icon.

## CLI

```
./bin/docked-browser build|run|activate|pause|resume|stop|delete [profile]
./bin/docked-browser pause-all|resume-all|stop-all
./bin/docked-browser refresh-dock [profile]
./bin/docked-browser focus|status
./bin/docked-predict-sleep          # LinUCB auto-pause daemon (also started by GUI/tray)
```

`activate` must stay safer than `run` (resume if paused, no-op if running, else run).
`delete` removes the container **and** `~/.config/docker-chrome-profiles/<name>/` (irreversible).

## Predict sleep (per profile)

Port of ambient-sleep’s disjoint LinUCB — actions are **KeepRunning** / **Pause** instead of OS sleep.

- Poll every **60s**; pause only after ≥**60s** unused idle; **5 min** breaker forces pause.
- In use = that profile's Chrome window is focused (window PID ↔ `docker top`). Container CPU is not activity. A focused window is not auto-paused.
- Either path shows a **20s** warning first; cancel starts a **15 min** grace.
- Open / Resume always target the **exact** profile via `activate` + `mark_used`.
- Checkpoint: `~/.local/share/docked-browser/linucb.json`
- Toggle: Advanced → Predict sleep, or `POST /api/predict-sleep` `{enabled}`.

## Usual-time suggestion

The same predict-sleep daemon (`gui/habit_suggest.py`, ticked from `gui/predict_sleep.py`) logs real use. A use is a focused docked window, or `mark_used` from a dock click, Open, or Resume. A profile sitting in the background does not count, and keyboard idle alone does not count.

Each use stores the profile, Malaysia weekday, and hour in `~/.local/share/docked-browser/habit.json` (about eight ISO weeks). For the current weekday and hour, the window is this ISO week and the three before it. If one profile was used in that slot in at least 3 of those 4 weeks, and it was used in more weeks than any other profile, a notification offers to open it. One notification per slot per Malaysia day; it expires after about **5 minutes** (`SUGGEST_WAIT_S`). No offer when that profile is already the focused window, or when the slot is not that consistent.

Click runs `bin/docked-browser activate <profile>` and returns immediately. That command resumes a paused profile and opens the workspace in `~/.config/docked-browser/workspaces.json` when the name is listed. Dismissing or missing the notification is not a use. This is separate from the pause warning (“pausing in 20s — click to keep awake”).

The profile panel shows a larger For now row for the current slot's winner when that profile is paused or not open; the profile remains in the list; a running (resumed) profile is not featured.
