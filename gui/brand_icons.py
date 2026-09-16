#!/usr/bin/env python3
"""Shared Docked Browser brand icons (web / tray / modal).

One family: Apple-blue rounded tile + browser chrome.
Small glyph differences so each surface is recognizable at a glance:

  web   — list rows (settings / full UI)
  tray  — menu bars (status area menu)
  modal — focus target (quick picker)

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
WHITE = (255, 255, 255, 255)
TITLE = (29, 29, 31, 255)
CONTENT = (245, 245, 247, 255)
DOT_R = (255, 95, 87, 255)
DOT_Y = (255, 189, 46, 255)
DOT_G = (40, 200, 64, 255)


def rounded_tile(size: int) -> Image.Image:
    """Blue rounded square with a soft top highlight."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    pad = max(1, size // 32)
    radius = max(4, size * 28 // 128)
    # Base fill
    draw.rounded_rectangle(
        [pad, pad, size - pad - 1, size - pad - 1],
        radius=radius,
        fill=BLUE,
    )
    # Subtle top gloss (second lighter rect, clipped visually by overlap)
    gloss = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    gdraw = ImageDraw.Draw(gloss)
    gdraw.rounded_rectangle(
        [pad, pad, size - pad - 1, size // 2],
        radius=radius,
        fill=(255, 255, 255, 36),
    )
    img = Image.alpha_composite(img, gloss)
    return img


def draw_browser_chrome(draw: ImageDraw.ImageDraw, size: int) -> tuple[int, int, int, int]:
    """Draw window + traffic lights. Returns content box (x0,y0,x1,y1)."""
    margin = size * 18 // 128
    x0, y0 = margin, margin + size // 32
    x1, y1 = size - margin - 1, size - margin - 1
    win_r = max(3, size * 10 // 128)
    draw.rounded_rectangle([x0, y0, x1, y1], radius=win_r, fill=WHITE)

    title_h = max(6, size * 22 // 128)
    # Title bar (flat top of window)
    draw.rounded_rectangle([x0, y0, x1, y0 + title_h + win_r], radius=win_r, fill=TITLE)
    draw.rectangle([x0, y0 + title_h, x1, y0 + title_h + win_r], fill=TITLE)

    # Traffic lights
    cy = y0 + title_h // 2
    r = max(1, size * 4 // 128)
    gap = max(3, size * 10 // 128)
    cx = x0 + gap + r
    for color in (DOT_R, DOT_Y, DOT_G):
        draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=color)
        cx += gap

    content = (x0 + 2, y0 + title_h + 2, x1 - 2, y1 - 2)
    # Content fill
    draw.rectangle([content[0], content[1], content[2], content[3]], fill=CONTENT)
    return content


def glyph_web(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """Two list rows — reads as ‘full UI / panels’."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    row_h = max(3, h // 5)
    pad_x = max(2, w // 8)
    gap = max(2, h // 10)
    y = y0 + (h - (row_h * 2 + gap)) // 2
    for _ in range(2):
        draw.rounded_rectangle(
            [x0 + pad_x, y, x1 - pad_x, y + row_h],
            radius=max(1, row_h // 3),
            fill=BLUE,
        )
        # small leading square (row icon)
        s = max(2, row_h - 2)
        draw.rounded_rectangle(
            [x0 + pad_x + 1, y + 1, x0 + pad_x + s, y + s],
            radius=1,
            fill=WHITE,
        )
        y += row_h + gap


def glyph_tray(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """Three menu bars — reads as ‘status menu’."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    bar_h = max(2, h // 9)
    pad_x = max(3, w // 6)
    gap = max(2, h // 8)
    total = bar_h * 3 + gap * 2
    y = y0 + (h - total) // 2
    widths = (1.0, 0.72, 0.55)  # taper like a menu
    for frac in widths:
        bw = int(w * frac) - 2 * pad_x
        bx0 = x0 + pad_x
        draw.rounded_rectangle(
            [bx0, y, bx0 + max(bar_h * 2, bw), y + bar_h],
            radius=max(1, bar_h // 2),
            fill=BLUE,
        )
        y += bar_h + gap


def glyph_modal(draw: ImageDraw.ImageDraw, box: tuple[int, int, int, int]) -> None:
    """Focus target — reads as ‘picker / bring to front’."""
    x0, y0, x1, y1 = box
    cx = (x0 + x1) // 2
    cy = (y0 + y1) // 2
    r_outer = max(4, min(x1 - x0, y1 - y0) * 28 // 100)
    stroke = max(2, r_outer // 5)
    for r in (r_outer, r_outer * 2 // 3):
        draw.ellipse(
            [cx - r, cy - r, cx + r, cy + r],
            outline=BLUE,
            width=stroke,
        )
    r_dot = max(2, r_outer // 4)
    draw.ellipse([cx - r_dot, cy - r_dot, cx + r_dot, cy + r_dot], fill=BLUE)


GLYPHS = {
    "web": glyph_web,
    "tray": glyph_tray,
    "modal": glyph_modal,
}


def render(variant: str, size: int = 128) -> Image.Image:
    if variant not in GLYPHS:
        raise SystemExit(f"Unknown variant {variant!r}; use {sorted(GLYPHS)}")
    img = rounded_tile(size)
    draw = ImageDraw.Draw(img)
    box = draw_browser_chrome(draw, size)
    GLYPHS[variant](draw, box)
    return img


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


FAVICON_SVG = """\
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" role="img" aria-label="Docked Browser">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#2b8af0"/>
      <stop offset="100%" stop-color="#0071e3"/>
    </linearGradient>
  </defs>
  <rect width="32" height="32" rx="7" fill="url(#g)"/>
  <rect x="5" y="7" width="22" height="18" rx="3" fill="#ffffff"/>
  <path d="M5 10.5h22V10a3 3 0 0 0-3-3H8a3 3 0 0 0-3 3v.5z" fill="#1d1d1f"/>
  <circle cx="8.2" cy="8.7" r="1" fill="#ff5f57"/>
  <circle cx="11.2" cy="8.7" r="1" fill="#ffbd2e"/>
  <circle cx="14.2" cy="8.7" r="1" fill="#28c840"/>
  <rect x="8" y="14" width="16" height="3.2" rx="1" fill="#0071e3"/>
  <rect x="8" y="19.2" width="12" height="3.2" rx="1" fill="#0071e3"/>
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
