# Docked Browser

**Chrome profiles with clearer cues — so you can focus on one job at a time.**

Same idea as Chrome’s built-in profiles (separate cookies/logins per name). Docked Browser adds a **dock icon per profile**, pause/resume, an optional focus modal/tray, and a tiny local web UI — without touching your normal host Chrome.

**Unofficial.** Not affiliated with Google. Chrome is a trademark of Google LLC.

**One common use case:** keep work / personal / shopping as separate windows so you stay in one context instead of tab-juggling. Other people use it differently — pick whatever names fit your head.

### Why use it
- **One dock icon per profile** — left-click opens that profile; right-click Pause / Resume targets that profile only
- **Pause** — freeze a window in RAM (almost no CPU) and come back later
- **Predict sleep** — unused profiles auto-pause (LinUCB, same formula as ambient-sleep)
- **Usual profile** — after a few weeks, a notification can offer the profile you use at this weekday and hour. Click it to open that profile.
- **Focus modal / tray / hotkey** — pick profiles without hunting windows
- **Leaves host Chrome alone** — runs in Docker beside your everyday browser
- **Tiny control UI** — local page on `127.0.0.1` (light / dark / system theme)

### Example use cases
- Focus mode: only the “work” window open while deep-working
- Personal vs shopping kept in other windows until you choose them
- Park a heavy tab set with **Pause** instead of quitting

### Limits
- Needs **Docker** (and a desktop session to show windows)
- **Not a new kind of security** — it’s profile separation, not a VPN or VM
- **Pause does not free RAM** — it only stops CPU
- Dock icons are never dimmed or swapped on pause (GNOME would cache a dimmed window icon)
- A paused profile leaves the Dash until resume (needs the Focused Window helper patch loaded at login)
- Profiles still share your machine, IP, and `~/Downloads`

**Supported host (tested):** Ubuntu **26.04** LTS, **GNOME** on **Wayland** (GNOME Shell 50), with Docker. NVIDIA is optional.

Other desktops (Cinnamon, Plank, X11, other distros) are **not supported** and **not tested**. Some leftover code paths may still exist in the tree; do not treat them as a product promise.

## Quick start

```bash
# 1) Build the image (once)
./bin/docked-browser build

# 2) Optional GPU
./bin/install-nvidia-toolkit.sh

# 3) Open a profile (each name gets its own dock icon)
./bin/docked-browser run work

# 4) Optional: tray + hotkey + web UI
./bin/install-autostart
./bin/install-hotkey          # Super+Shift+D → focus modal
./bin/start-gui               # http://127.0.0.1:8787
```

CLI:

```bash
./bin/docked-browser build
./bin/docked-browser run work
./bin/docked-browser activate work    # resume / focus / or run (what dock clicks use)
./bin/docked-browser pause work
./bin/docked-browser resume work
./bin/docked-browser stop work
./bin/docked-browser delete work      # erase that profile’s data (permanent)
./bin/docked-browser pause-all
./bin/docked-browser resume-all
./bin/docked-browser stop-all
./bin/docked-browser refresh-dock     # rebuild each profile’s dock icon
./bin/docked-browser focus            # profile picker (Profiles… on the dock menu)
./bin/docked-browser status           # JSON status
```

**Dock (GNOME Dash):** one icon per profile (`Docked · name`). Left-click runs `activate` for that name. Right-click always lists **Pause** and **Resume** for that profile (GNOME Shell freezes `Actions=` from the first load, so the menu does not swap them by container state). Profiles…, Resume all, and Close all appear when they apply. Pin icons from the Dash if you want. A paused profile leaves the Dash until resume (`SetDashHidden` on the Focused Window helper; the running Shell loads that method only after the next login).

## Workspaces (GNOME)

You create the workspaces yourself. The app does not create workspaces or change Mutter settings. Assignment is only `~/.config/docked-browser/workspaces.json`. Keys are workspace numbers starting at 1. Names must match the profile exactly (case-sensitive). Several profiles can share one number:

```json
{ "1": ["work", "personal"], "2": ["shopping"] }
```

`activate`, `resume`, and `run` open that workspace when the profile is listed. `resume-all` does not switch. A missing file, an empty file, or a profile that is not listed only raises the window. If the number is past the workspaces that exist, the script prints that the workspace does not exist and still raises the window.

The app patches the [Focused Window](https://github.com/flexagoon/focused-window-dbus) helper (`focused-window-dbus@flexagoon.com`) for `ActivateClass`, `SetDashHidden`, and `OpenWorkspace`. The running Shell does not load new methods until the next login. Until then, select and resume only raise the window (and paused profiles may stay visible on the Dash).

## Layout

```
.
├── bin/
│   ├── docked-browser           # CLI + dock entries
│   ├── focus-modal              # optional GTK picker
│   ├── docked-tray              # optional tray
│   ├── docked-predict-sleep     # LinUCB auto-pause daemon
│   ├── start-gui
│   ├── install-autostart
│   ├── install-hotkey
│   └── install-nvidia-toolkit.sh
├── gui/
│   ├── control.py
│   ├── server.py
│   ├── predict_sleep.py         # per-profile LinUCB (ambient-sleep port)
│   ├── habit_suggest.py         # weekday/hour profile suggestion
│   ├── wake_paused.py           # leftover X11 helper (unsupported)
│   ├── dock_icon.py             # per-profile GUI badges (web / modal)
│   ├── brand_icons.py           # unified web/tray/modal brand glyphs
│   ├── templates/
│   └── static/                  # favicons = web brand glyph
├── fontconfig/
├── Dockerfile
├── LICENSE
├── README.md
├── AGENTS.md
└── PLANK.md                     # raise / dock internals; Plank paths untested
```

## Data on disk (host)

| Location | Contents |
|----------|----------|
| `~/.config/docker-chrome-profiles/<name>/` | Chrome profile data |
| `…/<name>/dock-icon.png` | Optional custom dock icon source |
| `~/.local/share/docked-browser/icons/docked-browser.png` | Brand glyph (web / tray / modal) |
| `…/docked-browser-<name>.png` | That profile’s dock icon and GUI badge |
| `…/docked-browser-tray.png` / `-modal.png` | Tray / focus brand glyphs |
| `~/.local/share/applications/docked-browser-<name>.desktop` | That profile’s dock entry |
| `~/.config/docked-browser/workspaces.json` | Profiles listed under each GNOME workspace number |
| `~/.local/share/docked-browser/prefs.json` | Shared UI prefs (theme, advanced) |
| `~/.local/share/docked-browser/linucb.json` | Predict-sleep LinUCB checkpoint |
| `~/.local/share/docked-browser/predict-sleep-state.json` | Per-profile idle / pending |
| `~/.local/share/docked-browser/habit.json` | Weekday and hour each profile was actually used |
| `~/Downloads` | Shared downloads (read/write) |
| `$HOME` (read-only in container) | Same host paths for file uploads (WhatsApp, etc.) |

Naming:

- Profile: `^[a-zA-Z0-9_-]{1,64}$`
- Container: `chrome-<name>`
- WM class / `.desktop`: `docked-browser-<name>`

## Pause vs stop

- **Pause** (`docker pause`): frozen in **RAM**, almost no CPU — does **not** free memory.
- **Predict sleep**: unused running profiles are paused automatically (60s AI floor, 5 min hard breaker, 20s warning). Resume / Open always wake the **exact** named profile.
- **Usual time**: the same daemon can notify you to open the profile you used in this weekday and hour in at least 3 of the last 4 weeks. Click runs `activate` for that profile. One offer per slot per Malaysia day; the notification stays up about five minutes. Missing it does not count as use. The profile panel shows a larger For now row for the current slot's winner when that profile is paused or not open; the profile remains in the list; a running profile is not featured.
- **Stop / Close**: removes the container; profile data on disk is kept.
- **Delete**: stop + erase that profile’s data directory and custom icon (permanent).

## Dependencies

- Ubuntu 26.04 LTS + GNOME on Wayland (tested)
- Docker
- Python 3 + Pillow
- Optional for modal/tray: GTK 3 + Ayatana AppIndicator
- Optional: NVIDIA Container Toolkit
- Focused Window Shell extension (`focused-window-dbus@flexagoon.com`), patched in place by `bin/docked-browser` for raise / Dash hide / workspace switch

## Docs for contributors / AIs

See **[AGENTS.md](./AGENTS.md)**. Dock / raise internals: **[PLANK.md](./PLANK.md)** (filename is historical; Plank is not a supported target).

## License

[MIT](./LICENSE)
