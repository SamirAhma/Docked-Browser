#!/usr/bin/env python3
"""Build a static 128×128 PNG badge for a Docked Browser profile.

Used as that profile’s Dash icon and in the web UI / focus modal.

CLI (called by bin/docked-browser ensure_profile_gui_icon):
    dock_icon.py <name> <hex_color> <custom_path_or_empty> <output_png>

- If custom_path exists → that image, clipped to the same rounded tile,
  with a profile-color rim so it stays readable on the Dash.
- Else → rounded tile in hex_color with a heavy monogram. The full name is
  kept when it still fits at Dash size; longer names use two letters.

Do NOT generate live/paused variants. GNOME does not reliably update dock icons
for already-open windows when Icon= changes (see AGENTS.md).
"""
from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

SIZE = 128
# Draw large, then downsample, so 16–48px Dash/GUI sizes stay crisp.
SUPERSAMPLE = 4
RADIUS_AT_128 = 28
# Full name only when the glyphs stay at least this tall on the 128 canvas
# (~14px once GNOME scales the icon to ~48px).
MIN_FULL_NAME_PX = 40

FONT_CANDIDATES: tuple[tuple[str, int | None], ...] = (
    ("/usr/share/fonts/truetype/inter/Inter-Black.ttf", None),
    ("/usr/share/fonts/truetype/inter/Inter-ExtraBold.ttf", None),
    ("/usr/share/fonts/truetype/inter/Inter-Bold.ttf", None),
    (str(Path.home() / ".local/share/fonts/Inter/Inter.ttc"), 1),  # Inter Black
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", None),
    ("/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf", None),
)


def load_font(size_px: int) -> ImageFont.ImageFont:
    for path, index in FONT_CANDIDATES:
        try:
            if index is None:
                return ImageFont.truetype(path, size_px)
            return ImageFont.truetype(path, size_px, index=index)
        except OSError:
            continue
    return ImageFont.load_default()


def _channel(c: float) -> float:
    c = c / 255.0
    if c <= 0.04045:
        return c / 12.92
    return ((c + 0.055) / 1.055) ** 2.4


def luminance(color: tuple[int, int, int, int]) -> float:
    r, g, b = color[:3]
    return 0.2126 * _channel(r) + 0.7152 * _channel(g) + 0.0722 * _channel(b)


def contrast_ratio(fg: tuple[int, int, int, int], bg: tuple[int, int, int, int]) -> float:
    lighter = max(luminance(fg), luminance(bg))
    darker = min(luminance(fg), luminance(bg))
    return (lighter + 0.05) / (darker + 0.05)


def ink_for(fill: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    """White or near-black, whichever stays readable on this tile."""
    white = (255, 255, 255, 255)
    black = (20, 20, 22, 255)
    # Small Dash sizes are not “large text”, so aim for body-text contrast.
    if contrast_ratio(white, fill) >= 4.5:
        return white
    return black


def shade(color: tuple[int, int, int, int], factor: float) -> tuple[int, int, int, int]:
    r, g, b, a = color
    return tuple(max(0, min(255, int(round(c * factor)))) for c in (r, g, b)) + (a,)


def _downsample(img: Image.Image) -> Image.Image:
    return img.resize((SIZE, SIZE), Image.Resampling.LANCZOS)


def _rounded_tile(fill: tuple[int, int, int, int]) -> Image.Image:
    s = SIZE * SUPERSAMPLE
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    radius = RADIUS_AT_128 * SUPERSAMPLE
    # Darker rim so pale profile colors still separate from a light dock.
    draw.rounded_rectangle([0, 0, s - 1, s - 1], radius=radius, fill=shade(fill, 0.62))
    rim = max(2, int(round(2.5 * SUPERSAMPLE)))
    draw.rounded_rectangle(
        [rim, rim, s - 1 - rim, s - 1 - rim],
        radius=max(1, radius - rim),
        fill=fill,
    )
    return img


def _text_size(draw: ImageDraw.ImageDraw, text: str, font: ImageFont.ImageFont) -> tuple[int, int, int, int]:
    return draw.textbbox((0, 0), text, font=font)


def _fit_font(
    draw: ImageDraw.ImageDraw,
    text: str,
    max_width: int,
    max_height: int,
    start_px: int,
    stop_px: int,
) -> ImageFont.ImageFont | None:
    px = start_px
    while px >= stop_px:
        font = load_font(px)
        bbox = _text_size(draw, text, font)
        if bbox[2] - bbox[0] <= max_width and bbox[3] - bbox[1] <= max_height:
            return font
        px -= 2 * SUPERSAMPLE
    return None


def badge_label(name: str, draw: ImageDraw.ImageDraw, max_width: int, max_height: int) -> tuple[str, ImageFont.ImageFont]:
    """Prefer the full name; fall back to a two-letter mark when it would shrink away."""
    full = name.strip() or "?"
    min_px = MIN_FULL_NAME_PX * SUPERSAMPLE
    font = _fit_font(draw, full, max_width, max_height, start_px=max_height, stop_px=min_px)
    if font is not None:
        return full, font
    short = full if len(full) <= 3 else full[:2].upper()
    font = _fit_font(draw, short, max_width, max_height, start_px=max_height, stop_px=12 * SUPERSAMPLE)
    if font is None:
        font = load_font(max(12 * SUPERSAMPLE, max_height // 2))
    return short, font


def _draw_centered(draw: ImageDraw.ImageDraw, canvas: int, text: str, font: ImageFont.ImageFont, fill: tuple[int, int, int, int]) -> None:
    bbox = _text_size(draw, text, font)
    tw = bbox[2] - bbox[0]
    th = bbox[3] - bbox[1]
    x = (canvas - tw) / 2 - bbox[0]
    y = (canvas - th) / 2 - bbox[1]
    draw.text((x, y), text, font=font, fill=fill)


def name_badge(name: str, fill: tuple[int, int, int, int]) -> Image.Image:
    img = _rounded_tile(fill)
    draw = ImageDraw.Draw(img)
    s = SIZE * SUPERSAMPLE
    # Keep glyphs inside the curve of the squircle.
    max_width = int(s * 0.78)
    max_height = int(s * 0.62)
    label, font = badge_label(name, draw, max_width, max_height)
    _draw_centered(draw, s, label, font, ink_for(fill))
    return _downsample(img)


def _cover(src: Image.Image, side: int) -> Image.Image:
    """Scale so the picture fills a square, keeping the center."""
    sw, sh = src.size
    scale = max(side / sw, side / sh)
    nw = max(side, int(round(sw * scale)))
    nh = max(side, int(round(sh * scale)))
    resized = src.resize((nw, nh), Image.Resampling.LANCZOS)
    left = (nw - side) // 2
    top = (nh - side) // 2
    return resized.crop((left, top, left + side, top + side))


def from_custom(path: Path, fill: tuple[int, int, int, int]) -> Image.Image:
    """Custom art filling the same squircle, with a profile-color rim."""
    tile = _rounded_tile(fill)
    src = Image.open(path).convert("RGBA")
    s = SIZE * SUPERSAMPLE
    frame = 6 * SUPERSAMPLE
    side = s - 2 * frame
    covered = _cover(src, side)
    mask = Image.new("L", (side, side), 0)
    inner_r = max(1, RADIUS_AT_128 * SUPERSAMPLE - frame)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, side - 1, side - 1], radius=inner_r, fill=255)
    covered.putalpha(ImageChops.multiply(covered.getchannel("A"), mask))
    tile.alpha_composite(covered, (frame, frame))
    return _downsample(tile)


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
    img = from_custom(custom_path, fill) if custom_path.is_file() else name_badge(name, fill)
    out_path = Path(out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    img.save(out_path, format="PNG")


if __name__ == "__main__":
    main()
