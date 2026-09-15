# Docked Browser

**Chrome profiles with clearer cues — so you can focus on one job at a time.**

Same idea as Chrome’s built-in profiles (separate cookies/logins per name). Docked Browser adds named windows, custom dock icons, pause/resume, and a tiny local UI — without touching your normal host Chrome.

**Unofficial.** Not affiliated with Google. Chrome is a trademark of Google LLC.

**One common use case:** keep work / personal / shopping as separate windows so you stay in one context instead of tab-juggling. Other people use it differently (clients, experiments, parking heavy sessions) — pick whatever names fit your head.

### Why use it
- **Focus cues** — one named window per job; switch deliberately instead of drowning in tabs
- **Easy to tell apart** — custom dock icons so you open the right Chrome
- **Pause** — freeze a window in RAM (almost no CPU) and come back later
- **Leaves host Chrome alone** — runs in Docker beside your everyday browser
- **Tiny control UI** — local page on `127.0.0.1`

### Example use cases
- Focus mode: only the “work” window open while deep-working
- Personal vs shopping kept in other windows until you choose them
- Park a heavy tab set with **Pause** instead of quitting
- Try extensions or sites without cluttering everyday Chrome

### Limits
- Needs **Docker** (and a desktop session to show windows)
- **Not a new kind of security** — it’s profile separation, not a VPN or VM
- **Pause does not free RAM** — it only stops CPU
- Dock icons are **static** — GNOME won’t reliably dim them when paused
- Profiles still share your machine, IP, and `~/Downloads`

Target host: Linux + Docker (tested on Lubuntu / GNOME Wayland + NVIDIA).

## Quick start

```bash
# 1) Build the image (once)
./bin/docked-browser build

# 2) Optional: NVIDIA GPU via Container Toolkit (needs sudo in a real terminal)
./bin/install-nvidia-toolkit.sh

# 3) Start the GUI (http://127.0.0.1:8787)
./bin/start-gui
```

Or from the CLI:

```bash
./bin/docked-browser run work
./bin/docked-browser pause work
./bin/docked-browser resume work
./bin/docked-browser stop work
```

Autostart on login:

```bash
./bin/install-autostart
```

## Layout

```
.
├── bin/                         # Shell entrypoints
│   ├── docked-browser           # build / run / pause / resume / stop / dock
│   ├── start-gui                # local web UI launcher
│   ├── install-autostart
│   └── install-nvidia-toolkit.sh
├── gui/                         # Web UI (stdlib HTTP + Alpine.js)
│   ├── server.py
│   ├── dock_icon.py
│   ├── templates/
│   └── static/
├── fontconfig/                  # Fonts inside the Chrome image
├── Dockerfile
├── README.md
└── AGENTS.md                    # Read first if you are an AI editing this
```

## Data on disk (host)

| Location | Contents |
|----------|----------|
| `~/.config/docker-chrome-profiles/<name>/` | Chrome profile (cookies, extensions, …) |
| `…/<name>/dock-icon.png` | Optional custom icon source |
| `~/.local/share/docked-browser/icons/<class>.png` | Installed dock PNG |
| `~/.local/share/applications/docked-browser-<name>.desktop` | Dock / launcher entry |
| `~/Downloads` | Shared downloads mount |

Naming:

- Profile: `^[a-zA-Z0-9_-]{1,64}$`
- Container: `chrome-<name>` (internal; kept for compatibility)
- WM class / `.desktop`: `docked-browser-<name>`

## Pause vs stop

- **Pause** (`docker pause`): process frozen in **RAM**. Almost no CPU. Does **not** free memory or dump to disk.
- **Stop**: removes the container. Profile data on disk is kept.

## Dependencies

- Docker
- Python 3 + Pillow (`python3-pil` / `pip install Pillow`) for dock icons and non-PNG uploads
- Optional: `notify-send`, `zenity`, NVIDIA driver + Container Toolkit

## Docs for contributors / AIs

See **[AGENTS.md](./AGENTS.md)** for hard constraints (dock icons, GPU, hicolor, architecture).

## License

[MIT](./LICENSE)
