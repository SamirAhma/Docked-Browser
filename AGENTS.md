# AGENTS.md — Docked Browser

Instructions for AI assistants (and humans) editing this repo.
Read this before changing dock icons, GPU mounts, pause behaviour, or the GUI.

## What this project is

Local **Chrome-in-Docker** cockpit:

```
browser UI (gui/templates/index.html + Alpine.js)
        │  HTTP 127.0.0.1:8787
        ▼
   gui/server.py  ──subprocess──►  bin/docked-browser (bash)
                                         │
                         ┌───────────────┼────────────────┐
                         ▼               ▼                ▼
                   docker run/…    .desktop files    gui/dock_icon.py → PNG
                         ▼
                 Chrome container (image: local-docked-browser)
```

- **Do not** reintroduce Flask or other web frameworks — keep stdlib `http.server`.
- **Do not** call `docker run` from `gui/server.py` — always go through `bin/docked-browser`.
- GUI is beginner-oriented; keep copy plain and short.

## File map

| File | Edit when… |
|------|------------|
| `bin/docked-browser` | Docker flags, Wayland/X11, Pulse, GPU, dock `.desktop`, CLI commands |
| `gui/server.py` | HTTP API, status, icon upload |
| `gui/templates/index.html` | UI layout / Alpine state |
| `gui/dock_icon.py` | How the static PNG looks |
| `Dockerfile` | Packages/fonts inside the Chrome image (repo root) |
| `bin/start-gui` | Port / GUI launcher |
| `bin/install-autostart` | Writes `~/.config/autostart/docked-browser.desktop` with local absolute paths |
| `bin/install-nvidia-toolkit.sh` | Host NVIDIA Container Toolkit install |

## Hard constraints (lessons already learned)

### 1. Dock icons are static — do not try to dim on pause

Tried and **abandoned**:

- Swapping `Icon=` between live / paused PNGs
- Versioned filenames to bust caches
- Overlay “PAUSED” banners for open windows

**Why it fails on GNOME:**

- Dock caches the **window** icon when the app opens (`StartupWMClass` → `.desktop`).
- Changing `Icon=` later usually does **not** update an already-open window.
- Chrome’s green notification badge works because Chrome updates its icon **in-process**. We are outside the process; `docker pause` also freezes Chrome so it cannot redraw.

**Correct behaviour:** one static icon per profile (custom `dock-icon.png` or name badge). Pause state lives in the Docked Browser UI + notifications + dock right-click actions.

### 2. Never put absolute icon paths under `hicolor`

- `Icon=/…/icons/hicolor/128x128/apps/….png` often shows **blank** in GNOME Dock.
- Install icons under: `~/.local/share/docked-browser/icons/<class>.png`
- **Never** invent a partial `~/.local/share/icons/hicolor/index.theme` — it breaks system icons across the desktop.

### 3. GPU: toolkit or `/dev/dri` only — no host-lib bind mounts

- Prefer NVIDIA Container Toolkit → `docker run --gpus all`.
- Fallback: `/dev/dri` (+ optional `/dev/nvidia*` device nodes).
- **Do not** bind-mount host NVIDIA `.so` trees into the Debian container — ABI mismatch crashes GPU/Chrome.
- Dead code removed: caching host nvidia libs under `~/.cache/docked-browser/nvidia-libs-*`. Do not bring that back.

### 4. Pause does not free RAM or move memory to disk

`docker pause` freezes cgroup processes. Memory stays allocated. There is no “put pause on HDD” feature. Document this; don’t fake it.

### 5. Wayland first on Wayland sessions

If `WAYLAND_DISPLAY` + socket exist, use `--ozone-platform=wayland` and mount the Wayland socket. Else X11. Tiny/broken windows were fixed by Wayland ozone, not by more X11 hacks.

### 6. Profile / container naming must stay consistent

| Concept | Pattern |
|---------|---------|
| Profile name | `^[a-zA-Z0-9_-]{1,64}$` |
| Data dir | `~/.config/docker-chrome-profiles/<name>/` |
| Container | `chrome-<name>` |
| WM class / desktop id | `docked-browser-<name>` |
| Custom icon source | `<data dir>/dock-icon.png` |
| Installed icon | `~/.local/share/docked-browser/icons/docked-browser-<name>.png` |

`--class` / `--name` on Chrome **must** match `StartupWMClass` in the `.desktop` file.

## CLI contract (`bin/docked-browser`)

```
./bin/docked-browser build
./bin/docked-browser run [profile]
./bin/docked-browser activate [profile]   # dock click: resume / notify / or run
./bin/docked-browser pause|resume|stop [profile]
./bin/docked-browser refresh-dock [profile]
```

`activate` must stay safer than `run` (don’t always `docker rm -f` + recreate).

## HTTP API (`gui/server.py`)

| Method | Path | Body / notes |
|--------|------|----------------|
| GET | `/` | HTML |
| GET | `/api/status` | image + containers + profiles + icons |
| GET | `/api/icon/<profile>` | custom PNG or 404 |
| POST | `/api/action` | `{action, profile?}` — build/run/pause/resume/stop |
| POST | `/api/icon` | `{profile, action: set\|clear, image?}` data-URL |

Env: `DOCKED_BROWSER_HOST` (default `127.0.0.1`), `DOCKED_BROWSER_PORT` (default `8787`).

## Safe change checklist

Before finishing an edit, verify you did **not**:

- [ ] Add pause/live icon swapping or dimming
- [ ] Write icons into `~/.local/share/icons/hicolor/…`
- [ ] Create/modify `hicolor/index.theme`
- [ ] Mount host NVIDIA library directories into the container
- [ ] Bypass `bin/docked-browser` from the GUI for `docker run`
- [ ] Claim pause frees RAM or dumps to disk

## Useful host commands

```bash
./bin/docked-browser build
./bin/docked-browser refresh-dock <profile>
./bin/start-gui
# stuck GUI port:
fuser -k 8787/tcp
# icon dir ownership (if earlier runs used root):
sudo chown -R "$USER" ~/.local/share/docked-browser
```
