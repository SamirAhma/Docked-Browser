#!/usr/bin/env python3
"""Docked Browser — local web GUI for dockerized Chrome profiles.

Stack
-----
- stdlib ``http.server`` only (no Flask). Frontend: Alpine.js in templates/index.html.
- All Docker / dock work goes through ``bin/docked-browser`` (never call docker here except status).
- Bind: DOCKED_BROWSER_HOST (default 127.0.0.1), DOCKED_BROWSER_PORT (default 8787).
  Legacy: COCKPIT_HOST / COCKPIT_PORT still accepted.

HTTP API
--------
GET  /              → HTML UI
GET  /api/status    → {image_built, containers[], profiles[], icons{}}
GET  /api/icon/<p>  → custom PNG bytes (404 if none)
POST /api/action    → {action: build|run|pause|resume|stop, profile?}
POST /api/icon      → {profile, action: set|clear, image?: data-URL}

Profile names: ``^[a-zA-Z0-9_-]{1,64}$``
Custom icons live at ``~/.config/docker-chrome-profiles/<name>/dock-icon.png``.
See AGENTS.md before changing dock / GPU / pause behaviour.
"""

from __future__ import annotations

import base64
import json
import os
import re
import subprocess
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlparse

# --- Constants ---------------------------------------------------------------
GUI_DIR = Path(__file__).resolve().parent
ROOT = GUI_DIR.parent
SCRIPT = ROOT / "bin" / "docked-browser"
TEMPLATE = GUI_DIR / "templates" / "index.html"
STATIC_DIR = GUI_DIR / "static"
PROFILES_DIR = Path.home() / ".config" / "docker-chrome-profiles"
IMAGE_NAME = "local-docked-browser"
LEGACY_IMAGE_NAME = "local-chrome-cockpit"
PROFILE_RE = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")
HOST = os.environ.get("DOCKED_BROWSER_HOST") or os.environ.get("COCKPIT_HOST", "127.0.0.1")
PORT = int(os.environ.get("DOCKED_BROWSER_PORT") or os.environ.get("COCKPIT_PORT", "8787"))
MAX_ICON_BYTES = 2 * 1024 * 1024  # 2 MB
ALLOWED_ACTIONS = frozenset({"build", "run", "pause", "resume", "stop"})

# Public static files (favicon etc.) — basename only, no path traversal
STATIC_FILES: dict[str, tuple[Path, str]] = {
    "/favicon.svg": (STATIC_DIR / "favicon.svg", "image/svg+xml"),
    "/favicon.ico": (STATIC_DIR / "favicon.ico", "image/x-icon"),
    "/favicon-16.png": (STATIC_DIR / "favicon-16.png", "image/png"),
    "/favicon-32.png": (STATIC_DIR / "favicon-32.png", "image/png"),
    "/apple-touch-icon.png": (STATIC_DIR / "apple-touch-icon.png", "image/png"),
}


# --- Session / subprocess ----------------------------------------------------
def desktop_env() -> dict[str, str]:
    """Env so Docked Browser can reach the active Wayland/X11 session."""
    env = os.environ.copy()
    uid = os.getuid()
    runtime = env.get("XDG_RUNTIME_DIR") or f"/run/user/{uid}"
    env.setdefault("XDG_RUNTIME_DIR", runtime)
    env.setdefault("DISPLAY", ":0")
    if not env.get("WAYLAND_DISPLAY") and Path(runtime, "wayland-0").exists():
        env["WAYLAND_DISPLAY"] = "wayland-0"
    return env


def run_cockpit(action: str, profile: str | None = None) -> dict:
    """Run ``bin/docked-browser <action> [profile]`` and return {ok, output}."""
    if not SCRIPT.is_file():
        return {"ok": False, "output": f"Missing script: {SCRIPT}"}

    cmd = [str(SCRIPT), action]
    if profile is not None:
        if not PROFILE_RE.match(profile):
            return {
                "ok": False,
                "output": "Invalid profile name. Use letters, numbers, _ or - (max 64).",
            }
        cmd.append(profile)

    try:
        result = subprocess.run(
            cmd,
            cwd=str(ROOT),
            env=desktop_env(),
            capture_output=True,
            text=True,
            timeout=600 if action == "build" else 60,
        )
    except subprocess.TimeoutExpired:
        return {"ok": False, "output": f"Timed out running: {' '.join(cmd)}"}
    except OSError as exc:
        return {"ok": False, "output": str(exc)}

    output = (result.stdout or "") + (result.stderr or "")
    return {"ok": result.returncode == 0, "output": output.strip() or "(no output)"}


# --- Docker status / profiles ------------------------------------------------
def docker_chrome_status() -> list[dict]:
    try:
        result = subprocess.run(
            [
                "docker",
                "ps",
                "-a",
                "--filter",
                "name=chrome-",
                "--format",
                "{{.Names}}\t{{.Status}}\t{{.State}}",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []

    rows = []
    for line in result.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        name, status, state = parts[0], parts[1], parts[2]
        if not name.startswith("chrome-"):
            continue
        profile = name[len("chrome-") :]
        rows.append(
            {
                "name": name,
                "profile": profile,
                "status": status,
                "state": state,
                "paused": state == "paused",
                "running": state == "running",
            }
        )
    return rows


def known_profiles() -> list[str]:
    profiles: set[str] = set()
    if PROFILES_DIR.is_dir():
        for path in PROFILES_DIR.iterdir():
            if path.is_dir() and PROFILE_RE.match(path.name):
                profiles.add(path.name)
    for row in docker_chrome_status():
        profiles.add(row["profile"])
    return sorted(profiles)


def image_built() -> bool:
    for name in (IMAGE_NAME, LEGACY_IMAGE_NAME):
        try:
            inspect = subprocess.run(
                ["docker", "image", "inspect", name],
                capture_output=True,
                text=True,
                timeout=15,
            )
            if inspect.returncode == 0:
                return True
        except (OSError, subprocess.TimeoutExpired):
            continue
    return False


def status_payload() -> dict:
    profiles = known_profiles()
    return {
        "image_built": image_built(),
        "containers": docker_chrome_status(),
        "profiles": profiles,
        "icons": {name: icon_path(name).is_file() for name in profiles},
    }


# --- Custom dock icons -------------------------------------------------------
def icon_path(profile: str) -> Path:
    return PROFILES_DIR / profile / "dock-icon.png"


def decode_data_url(data_url: str) -> bytes:
    if not data_url.startswith("data:") or "," not in data_url:
        raise ValueError("Expected a data: URL image")
    header, b64 = data_url.split(",", 1)
    if "image/" not in header.lower():
        raise ValueError("Only image uploads are allowed")
    raw = base64.b64decode(b64, validate=False)
    if len(raw) > MAX_ICON_BYTES:
        raise ValueError("Image too large (max 2 MB)")
    if len(raw) < 32:
        raise ValueError("Image too small")
    if not (
        raw.startswith(b"\x89PNG\r\n\x1a\n")
        or raw.startswith(b"\xff\xd8\xff")
        or (raw[:4] == b"RIFF" and raw[8:12] == b"WEBP")
        or raw.startswith(b"GIF87a")
        or raw.startswith(b"GIF89a")
    ):
        raise ValueError("Unsupported image type (use PNG, JPEG, WebP, or GIF)")
    return raw


def to_png_bytes(raw: bytes) -> bytes:
    """Keep PNG as-is; convert others via Pillow if available."""
    if raw.startswith(b"\x89PNG\r\n\x1a\n"):
        return raw
    try:
        from io import BytesIO

        from PIL import Image  # type: ignore

        img = Image.open(BytesIO(raw)).convert("RGBA")
        img.thumbnail((256, 256))
        out = BytesIO()
        img.save(out, format="PNG")
        return out.getvalue()
    except Exception as exc:
        raise ValueError(
            "Please upload a PNG icon (or install Pillow for JPEG/WebP)."
        ) from exc


def save_icon(profile: str, data_url: str) -> dict:
    if not PROFILE_RE.match(profile):
        return {"ok": False, "output": "Invalid profile name"}
    try:
        png = to_png_bytes(decode_data_url(data_url))
    except (ValueError, Exception) as exc:
        return {"ok": False, "output": str(exc)}

    dest = icon_path(profile)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(png)
    dock = run_cockpit("refresh-dock", profile)
    if not dock["ok"]:
        return {
            "ok": False,
            "output": f"Icon saved, but dock refresh failed:\n{dock['output']}",
        }
    return {
        "ok": True,
        "output": (
            f"Custom icon saved for “{profile}”. "
            "Re-open the window (or unpin/repin) if the dock still shows the old picture."
        ),
        **status_payload(),
    }


def clear_icon(profile: str) -> dict:
    if not PROFILE_RE.match(profile):
        return {"ok": False, "output": "Invalid profile name"}
    path = icon_path(profile)
    if path.is_file():
        path.unlink()
    dock = run_cockpit("refresh-dock", profile)
    return {
        "ok": dock["ok"],
        "output": dock["output"]
        if not dock["ok"]
        else f"Cleared custom icon for “{profile}” (back to name badge).",
        **status_payload(),
    }


# --- HTTP --------------------------------------------------------------------
class CockpitHandler(BaseHTTPRequestHandler):
    server_version = "DockedBrowser/1.0"

    def log_message(self, fmt: str, *args) -> None:
        print(f"[{self.log_date_time_string()}] {fmt % args}")

    def _send(self, code: int, body: bytes, content_type: str) -> None:
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _send_json(self, code: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self._send(code, body, "application/json; charset=utf-8")

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path == "/":
            try:
                html = TEMPLATE.read_text(encoding="utf-8").encode("utf-8")
            except OSError as exc:
                self._send(500, str(exc).encode(), "text/plain; charset=utf-8")
                return
            self._send(200, html, "text/html; charset=utf-8")
            return

        if path in STATIC_FILES:
            file_path, content_type = STATIC_FILES[path]
            try:
                body = file_path.read_bytes()
            except OSError as exc:
                self._send(404, str(exc).encode(), "text/plain; charset=utf-8")
                return
            # Favicons can be cached a bit; API stays no-store via _send_json
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "public, max-age=86400")
            self.end_headers()
            self.wfile.write(body)
            return

        if path == "/api/status":
            self._send_json(200, status_payload())
            return

        if path.startswith("/api/icon/"):
            profile = unquote(path[len("/api/icon/") :])
            if not PROFILE_RE.match(profile):
                self._send(400, b"bad profile", "text/plain")
                return
            icon = icon_path(profile)
            if not icon.is_file():
                self._send(404, b"no custom icon", "text/plain")
                return
            self._send(200, icon.read_bytes(), "image/png")
            return

        self._send(404, b"Not found", "text/plain; charset=utf-8")

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        length = int(self.headers.get("Content-Length", "0") or "0")
        raw = self.rfile.read(length) if length else b"{}"
        try:
            payload = json.loads(raw.decode("utf-8") or "{}")
        except json.JSONDecodeError:
            self._send_json(400, {"ok": False, "output": "Invalid JSON"})
            return

        if path == "/api/icon":
            profile = str(payload.get("profile", "")).strip()
            action = str(payload.get("action", "set")).strip().lower()
            if action == "clear":
                result = clear_icon(profile)
            else:
                result = save_icon(profile, str(payload.get("image", "")))
            self._send_json(200 if result["ok"] else 400, result)
            return

        if path != "/api/action":
            self._send(404, b"Not found", "text/plain; charset=utf-8")
            return

        action = str(payload.get("action", "")).strip().lower()
        profile = str(payload.get("profile", "default")).strip() or "default"

        if action not in ALLOWED_ACTIONS:
            self._send_json(400, {"ok": False, "output": "Unknown action"})
            return

        if action == "build":
            result = run_cockpit("build")
        else:
            result = run_cockpit(action, profile)

        result.update(status_payload())
        self._send_json(200 if result["ok"] else 500, result)


def main() -> None:
    if not TEMPLATE.is_file():
        raise SystemExit(f"Missing template: {TEMPLATE}")

    server = ThreadingHTTPServer((HOST, PORT), CockpitHandler)
    print(f"Docked Browser GUI → http://{HOST}:{PORT}")
    print("Press Ctrl+C to stop.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped.")
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
