#!/usr/bin/env python3
"""Build a static 128×128 PNG dock icon for a Docked Browser profile.

CLI (called by bin/docked-browser install_dock_entry):
    dock_icon.py <name> <hex_color> <custom_path_or_empty> <output_png>

- If custom_path exists → thumbnail that image onto a transparent 128² canvas.
- Else → rounded name badge filled with hex_color.

Do NOT generate live/paused variants. GNOME does not reliably update dock icons
for already-open windows when Icon= changes (see AGENTS.md).
"""
from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SIZE = 128
FONT_CANDIDATES = (
    "/usr/share/fonts/truetype/inter/Inter-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
)


def load_font(size_px: int) -> ImageFont.ImageFont:
    for path in FONT_CANDIDATES:
        try:
            return ImageFont.truetype(path, size_px)
        except OSError:
            continue
    return ImageFont.load_default()


def fit_text(draw: ImageDraw.ImageDraw, text: str, max_width: int) -> ImageFont.ImageFont:
    for px in (28, 24, 20, 16, 14, 12, 10):
        f = load_font(px)
        bbox = draw.textbbox((0, 0), text, font=f)
        if bbox[2] - bbox[0] <= max_width:
            return f
    return load_font(10)


def name_badge(name: str, fill: tuple[int, int, int, int]) -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw.rounded_rectangle([4, 4, SIZE - 5, SIZE - 5], radius=28, fill=fill)
    f = fit_text(draw, name, SIZE - 24)
    draw.text((SIZE // 2, SIZE // 2), name, fill=(255, 255, 255, 255), font=f, anchor="mm")
    return img


def from_custom(path: Path) -> Image.Image:
    src = Image.open(path).convert("RGBA")
    canvas = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    src.thumbnail((SIZE - 8, SIZE - 8))
    x = (SIZE - src.width) // 2
    y = (SIZE - src.height) // 2
    canvas.paste(src, (x, y), src)
    return canvas


def parse_hex_color(color: str) -> tuple[int, int, int, int]:
    color = color.lstrip("#")
    if len(color) != 6:
        raise SystemExit(f"Expected 6-digit hex color, got: {color!r}")
    return tuple(int(color[i : i + 2], 16) for i in (0, 2, 4)) + (255,)


def main() -> None:
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    name, color, custom, out = sys.argv[1:5]
    fill = parse_hex_color(color)
    custom_path = Path(custom)
    img = from_custom(custom_path) if custom_path.is_file() else name_badge(name, fill)
    out_path = Path(out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    img.save(out_path, format="PNG")


if __name__ == "__main__":
    main()
