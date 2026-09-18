# AGENTS.md — Docked Browser

Instructions for AI assistants (and humans) editing this repo.

## Architecture

```
per-profile Dash icon ──click──► bin/docked-browser activate <name>
web UI / tray / focus-modal ──────────► bin/docked-browser ──► docker Chrome
```

- Keep stdlib `http.server` for the full UI (no Flask).
- Lifecycle goes through `bin/docked-browser`.
- **Per-profile** WM class + dock icon. Click opens that profile. Right-click Actions rewrite with Docker state (only valid options).

## Naming

| Concept | Pattern |
|---------|---------|
| Profile | `^[a-zA-Z0-9_-]{1,64}$` |
| Data dir | `~/.config/docker-chrome-profiles/<name>/` |
| Container | `chrome-<name>` |
| WM class / desktop | `docked-browser-<name>` |
| Dock icon | `~/.local/share/docked-browser/icons/docked-browser-<name>.png` |

`--class` / `--name` must match `StartupWMClass`.

## Hard constraints

1. **Do not** try to dim dock icons on pause (GNOME caches window icons).
2. **Do not** put absolute icons under `hicolor` or invent `hicolor/index.theme`.
3. **No host NVIDIA `.so` bind-mounts** into the container.
4. **Pause does not free RAM.**
5. Optional focus modal/tray/hotkey are extras — dock click remains the primary UX.
6. Dock right-click `Actions=` are rewritten on lifecycle changes — only valid items for current Docker state.

## CLI

```
./bin/docked-browser build|run|activate|pause|resume|stop [profile]
./bin/docked-browser refresh-dock [profile]
./bin/docked-browser focus|status
```

`activate` must stay safer than `run`.
