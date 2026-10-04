#!/usr/bin/env python3
"""Shared Docked Browser brand icons (web / tray / modal).

One family: a blue rounded tile and a white browser window.
The mark inside the window is a few thick shapes so it still reads at 16px:

  web   — two content bars (list / full UI)
  tray  — three menu bars (status menu)
  modal — a focus ring (quick picker)

Usage:
  python3 brand_icons.py                 # write into ~/.local/share/… + gui/static
  python3 brand_icons.py --icons-dir DIR --static-dir DIR
"""
from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw

# Apple system blue (matches gui/templates)
BLUE = (0, 113, 227, 255)
BLUE_EDGE = (0, 74, 158, 255)
WHITE = (255, 255, 255, 255)
TITLE = (29, 29, 31, 255)


def _render_exact(variant: str, size: int) -> Image.Image:
    if variant not in GLYPHS:
        raise SystemExit(f"Unknown variant {variant!r}; use {sorted(GLYPHS)}")
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    radius = max(4, round(size * 0.22))
    # Darker edge so the tile separates from a light menu bar or dash.
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=BLUE_EDGE)
    rim = max(1, round(size * 0.02))
    draw.rounded_rectangle(
        [rim, rim, size - 1 - rim, size - 1 - rim],
        radius=max(2, radius - rim),
        fill=BLUE,
    )
    box = _window(draw, size)
    GLYPHS[variant](draw, box)
    return img


def _window(draw: ImageDraw.ImageDraw, size: int) -> tuple[int, int, int, int]:
    """White page with a heavy title band. Returns the content box."""
    margin = max(2, round(size * 0.10))
    x0 = margin
    y0 = max(2, round(size * 0.12))
    x1 = size - margin - 1
    y1 = size - max(2, round(size * 0.10)) - 1
    win_r = max(2, round(size * 0.07))
    draw.rounded_rectangle([x0, y0, x1, y1], radius=win_r, fill=WHITE)

    title_h = max(3, round((y1 - y0) * 0.26))
    # Square off the bottom of the title so the top corners stay rounded.
    draw.rounded_rectangle([x0, y0, x1, y0 + title_h + win_r], radius=win_r, fill=TITLE)
    draw.rectangle([x0, y0 + title_h, x1, y0 + title_h + win_r], fill=TITLE)

    pad = max(2, round(size * 0.035))
    return (x0 + pad, y0 + title_h + pad, x1 - pad, y1 - pad)


def glyph_web(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """Two thick bars — the full UI."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    bar_h = max(2, round(h * 0.28))
    gap = max(2, round(h * 0.16))
    total = bar_h * 2 + gap
    y = y0 + (h - total) // 2
    inset = max(1, round(w * 0.06))
    radius = max(1, bar_h // 2)
    for _ in range(2):
        draw.rounded_rectangle([x0 + inset, y, x1 - inset, y + bar_h], radius=radius, fill=BLUE)
        y += bar_h + gap


def glyph_tray(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """Three thick bars — a status menu."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    bar_h = max(2, round(h * 0.18))
    gap = max(1, round(h * 0.12))
    total = bar_h * 3 + gap * 2
    y = y0 + max(0, (h - total) // 2)
    inset = max(1, round(w * 0.10))
    radius = max(1, bar_h // 2)
    for _ in range(3):
        draw.rounded_rectangle([x0 + inset, y, x1 - inset, y + bar_h], radius=radius, fill=BLUE)
        y += bar_h + gap


def glyph_modal(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """One heavy ring and a solid core — bring this window forward."""
    x0, y0, x1, y1 = box
    cx = (x0 + x1) // 2
    cy = (y0 + y1) // 2
    r = max(4, round(min(x1 - x0, y1 - y0) * 0.42))
    stroke = max(2, round(r * 0.34))
    draw.ellipse([cx - r, cy - r, cx + r, cy + r], outline=BLUE, width=stroke)
    dot = max(2, round(r * 0.28))
    draw.ellipse([cx - dot, cy - dot, cx + dot, cy + dot], fill=BLUE)


GLYPHS = {
    "web": glyph_web,
    "tray": glyph_tray,
    "modal": glyph_modal,
}


def render(variant: str, size: int = 128) -> Image.Image:
    if variant not in GLYPHS:
        raise SystemExit(f"Unknown variant {variant!r}; use {sorted(GLYPHS)}")
    # Supersample so 16px favicons and tray icons keep smooth, solid shapes.
    scale = 4
    img = _render_exact(variant, size * scale)
    return img.resize((size, size), Image.Resampling.LANCZOS)


def write_png(path: Path, img: Image.Image) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path, format="PNG")


def write_ico(path: Path, sizes: tuple[int, ...] = (16, 32)) -> None:
    frames = [render("web", s) for s in sizes]
    path.parent.mkdir(parents=True, exist_ok=True)
    frames[0].save(
        path,
        format="ICO",
        sizes=[(s, s) for s in sizes],
        append_images=frames[1:],
    )


# Matches render("web") at small sizes: no traffic-light dots, two thick bars.
FAVICON_SVG = """\
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" role="img" aria-label="Docked Browser">
  <rect width="32" height="32" rx="7" fill="#004a9e"/>
  <rect x="1" y="1" width="30" height="30" rx="6.2" fill="#0071e3"/>
  <rect x="3" y="4" width="26" height="24" rx="2.5" fill="#ffffff"/>
  <path d="M3 8.5h26V6.5a2.5 2.5 0 0 0-2.5-2.5h-21A2.5 2.5 0 0 0 3 6.5v2z" fill="#1d1d1f"/>
  <rect x="6" y="14" width="20" height="4.2" rx="2.1" fill="#0071e3"/>
  <rect x="6" y="21" width="20" height="4.2" rx="2.1" fill="#0071e3"/>
</svg>
"""


def install(icons_dir: Path, static_dir: Path | None) -> None:
    mapping = {
        "web": "docked-browser.png",
        "tray": "docked-browser-tray.png",
        "modal": "docked-browser-modal.png",
    }
    for variant, name in mapping.items():
        write_png(icons_dir / name, render(variant, 128))
        print(f"Wrote {icons_dir / name}")

    if static_dir is not None:
        static_dir.mkdir(parents=True, exist_ok=True)
        (static_dir / "favicon.svg").write_text(FAVICON_SVG, encoding="utf-8")
        write_png(static_dir / "favicon-16.png", render("web", 16))
        write_png(static_dir / "favicon-32.png", render("web", 32))
        write_png(static_dir / "apple-touch-icon.png", render("web", 180))
        write_ico(static_dir / "favicon.ico")
        # Also keep copies of all three for docs / future use
        write_png(static_dir / "brand-web.png", render("web", 128))
        write_png(static_dir / "brand-tray.png", render("tray", 128))
        write_png(static_dir / "brand-modal.png", render("modal", 128))
        print(f"Wrote favicons + brand-* under {static_dir}")


def main() -> None:
    import os

    xdg = os.environ.get("XDG_DATA_HOME")
    default_icons = (
        Path(xdg) / "docked-browser" / "icons"
        if xdg
        else Path.home() / ".local" / "share" / "docked-browser" / "icons"
    )

    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--icons-dir", type=Path, default=default_icons)
    parser.add_argument("--static-dir", type=Path, default=here / "static")
    parser.add_argument("--no-static", action="store_true")
    args = parser.parse_args()
    install(args.icons_dir, None if args.no_static else args.static_dir)


if __name__ == "__main__":
    main()
