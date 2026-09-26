# Docked Browser

**Chrome profiles with clearer cues — so you can focus on one job at a time.**

Same idea as Chrome’s built-in profiles (separate cookies/logins per name). Docked Browser adds a **dock icon per profile**, pause/resume, an optional focus modal/tray, and a tiny local web UI — without touching your normal host Chrome.

**Unofficial.** Not affiliated with Google. Chrome is a trademark of Google LLC.

**One common use case:** keep work / personal / shopping as separate windows so you stay in one context instead of tab-juggling. Other people use it differently — pick whatever names fit your head.

### Why use it
- **One dock icon per profile** — click that icon to open it; right-click still has Profiles…, Pause all, and Close all
- **Pause** — freeze a window in RAM (almost no CPU) and come back later
- **Predict sleep** — unused profiles auto-pause (LinUCB, same formula as ambient-sleep)
- **Focus modal / tray / hotkey** — pick profiles without hunting windows
- **Leaves host Chrome alone** — runs in Docker beside your everyday browser
- **Tiny control UI** — local page on `127.0.0.1`

### Example use cases
- Focus mode: only the “work” window open while deep-working
- Personal vs shopping kept in other windows until you choose them
- Park a heavy tab set with **Pause** instead of quitting

### Limits
- Needs **Docker** (and a desktop session to show windows)
- **Not a new kind of security** — it’s profile separation, not a VPN or VM
- **Pause does not free RAM** — it only stops CPU
- Dock icons don’t dim when paused (GNOME limit)
- Profiles still share your machine, IP, and `~/Downloads`

Target host: Linux + Docker (tested on Lubuntu / GNOME Wayland + NVIDIA).

## Quick start

```bash
# 1) Build the image (once)
./bin/docked-browser build

# 2) Optional GPU
./bin/install-nvidia-toolkit.sh

# 3) Open a profile (each name gets its own dock icon you can pin)
./bin/docked-browser run work

# 4) Optional: tray + hotkey + web UI
./bin/install-autostart
./bin/install-hotkey          # Super+Shift+D → focus modal
./bin/start-gui               # http://127.0.0.1:8787
```

CLI:

```bash
./bin/docked-browser run work
./bin/docked-browser pause work
./bin/docked-browser resume work
./bin/docked-browser stop work
./bin/docked-browser refresh-dock      # rebuild each profile’s dock icon
./bin/docked-browser focus             # profile picker (also dock left-click)
```

**Dock:** one icon per profile (`Docked · name`). Left-click opens that profile. Right-click shows Profiles… plus Pause all / Resume all / Close all when any containers are live. Pin the ones you want from the Dash.

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
│   ├── dock_icon.py             # per-profile GUI badges (web / modal)
│   ├── brand_icons.py           # unified web/tray/modal brand glyphs
│   ├── templates/
│   └── static/                  # favicons = web brand glyph
├── fontconfig/
├── Dockerfile
├── README.md
└── AGENTS.md
```

## Data on disk (host)

| Location | Contents |
|----------|----------|
| `~/.config/docker-chrome-profiles/<name>/` | Chrome profile data |
| `…/<name>/dock-icon.png` | Optional custom dock icon source |
| `~/.local/share/docked-browser/icons/docked-browser.png` | Brand glyph (web / tray / modal) |
| `…/docked-browser-<name>.png` | That profile’s Dash icon and GUI badge |
| `…/docked-browser-tray.png` / `-modal.png` | Tray / focus brand glyphs |
| `~/.local/share/applications/docked-browser-<name>.desktop` | That profile’s dock entry |
| `~/.local/share/docked-browser/prefs.json` | Shared UI prefs (theme, advanced) |
| `~/.local/share/docked-browser/linucb.json` | Predict-sleep LinUCB checkpoint |
| `~/.local/share/docked-browser/predict-sleep-state.json` | Per-profile idle / pending |
| `~/Downloads` | Shared downloads (read/write) |
| `$HOME` (read-only in container) | Same host paths for file uploads (WhatsApp, etc.) |

Naming:

- Profile: `^[a-zA-Z0-9_-]{1,64}$`
- Container: `chrome-<name>`
- WM class / `.desktop`: `docked-browser-<name>`

## Pause vs stop

- **Pause** (`docker pause`): frozen in **RAM**, almost no CPU — does **not** free memory.
- **Predict sleep**: unused running profiles are paused automatically (60s AI floor, 10 min hard breaker). Resume / Open always wake the **exact** named profile.
- **Stop / Close**: removes the container; profile data on disk is kept.
- **Delete**: stop + erase that profile’s data directory and custom icon (permanent).

## Dependencies

- Docker
- Python 3 + Pillow
- Optional for modal/tray: GTK 3 + Ayatana AppIndicator
- Optional: NVIDIA Container Toolkit

## Docs for contributors / AIs

See **[AGENTS.md](./AGENTS.md)**.

## License

[MIT](./LICENSE)
