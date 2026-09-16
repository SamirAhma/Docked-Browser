# Docked Browser

**Chrome profiles with clearer cues — so you can focus on one job at a time.**

Same idea as Chrome’s built-in profiles (separate cookies/logins per name). Docked Browser adds **per-profile dock icons**, pause/resume, an optional focus modal/tray, and a tiny local web UI — without touching your normal host Chrome.

**Unofficial.** Not affiliated with Google. Chrome is a trademark of Google LLC.

**One common use case:** keep work / personal / shopping as separate windows so you stay in one context instead of tab-juggling. Other people use it differently — pick whatever names fit your head.

### Why use it
- **Per-profile dock icons** — click the icon, that Chrome opens (like a normal app)
- **Pause** — freeze a window in RAM (almost no CPU) and come back later
- **Optional focus modal / tray / hotkey** — pick profiles without hunting windows
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

# 3) Open a profile (creates a dock icon you can pin)
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
./bin/docked-browser refresh-dock      # rebuild all profile dock icons
./bin/docked-browser focus             # optional picker
```

**Dock:** each profile is its own app (`Docked · work`, etc.). Click = open that Chrome. No right-click actions — pause/resume from the web UI, tray, or CLI. Pin the ones you use from the app grid / Dash.

## Layout

```
.
├── bin/
│   ├── docked-browser           # CLI + dock entries
│   ├── focus-modal              # optional GTK picker
│   ├── docked-tray              # optional tray
│   ├── start-gui
│   ├── install-autostart
│   ├── install-hotkey
│   └── install-nvidia-toolkit.sh
├── gui/
│   ├── control.py
│   ├── server.py
│   ├── dock_icon.py
│   ├── templates/
│   └── static/
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
| `~/.local/share/docked-browser/icons/docked-browser-<name>.png` | Installed dock PNG |
| `~/.local/share/applications/docked-browser-<name>.desktop` | Dock / launcher entry |
| `~/Downloads` | Shared downloads mount |

Naming:

- Profile: `^[a-zA-Z0-9_-]{1,64}$`
- Container: `chrome-<name>`
- WM class / `.desktop`: `docked-browser-<name>`

## Pause vs stop

- **Pause** (`docker pause`): frozen in **RAM**, almost no CPU — does **not** free memory.
- **Stop**: removes the container; profile data on disk is kept.

## Dependencies

- Docker
- Python 3 + Pillow
- Optional for modal/tray: GTK 3 + Ayatana AppIndicator
- Optional: `notify-send`, NVIDIA Container Toolkit

## Docs for contributors / AIs

See **[AGENTS.md](./AGENTS.md)**.

## License

[MIT](./LICENSE)
