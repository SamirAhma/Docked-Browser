# AGENTS.md — Docked Browser

Instructions for AI assistants (and humans) editing this repo.

## Architecture

```
profile Dash icon ──click──► bin/docked-browser activate <name>
web UI / tray / focus-modal ──────────► bin/docked-browser ──► docker Chrome
```

- Keep stdlib `http.server` for the full UI (no Flask).
- Lifecycle goes through `bin/docked-browser`.
- **One Dash icon per profile** (`docked-browser-<name>`). Left-click activates that profile.
- Right-click `Actions=` is the same fleet menu on every icon (Profiles…; Pause/Resume/Close all when valid).

## Naming

| Concept | Pattern |
|---------|---------|
| Profile | `^[a-zA-Z0-9_-]{1,64}$` |
| Data dir | `~/.config/docker-chrome-profiles/<name>/` |
| Container | `chrome-<name>` |
| WM class / desktop | `docked-browser-<name>` |
| Profile Dash icon | `~/.local/share/docked-browser/icons/docked-browser-<name>.png` |
| Brand glyph (web / tray / modal) | `~/.local/share/docked-browser/icons/docked-browser.png` |

`--class` / `--name` must match that profile’s `StartupWMClass` (`docked-browser-<name>`).

## Hard constraints

1. **Do not** try to dim dock icons on pause (GNOME caches window icons).
2. **Do not** put absolute icons under `hicolor` or invent `hicolor/index.theme`.
3. **No host NVIDIA `.so` bind-mounts** into the container.
4. **Pause does not free RAM.**
5. Optional focus modal/tray/hotkey are extras. A profile’s dock click runs `activate`. Profiles… opens the focus picker.
6. Dock right-click `Actions=` are rewritten on every profile desktop when fleet state changes — only valid items (Profiles…; Pause/Resume/Close all).
7. Each profile keeps its own Dash icon. Do not merge windows back onto one shared `docked-browser` class.
8. A **paused** profile is removed from the Dash until it resumes. The window stays; do not dim or swap its icon.

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

- Poll every **60s**; pause only after ≥**60s** unused idle; **10 min** breaker forces pause.
- In use = that profile's Chrome window is focused (window PID ↔ `docker top`). Container CPU is not activity. A focused window is not auto-paused.
- Open / Resume always target the **exact** profile via `activate` + `mark_used`.
- Checkpoint: `~/.local/share/docked-browser/linucb.json`
- Toggle: Advanced → Predict sleep, or `POST /api/predict-sleep` `{enabled}`.
