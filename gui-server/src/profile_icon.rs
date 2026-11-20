use fontdue::{Font, FontSettings};
use image::{DynamicImage, ImageReader, Rgba, RgbaImage, imageops};
use std::io;
use std::path::Path;

const SIZE: u32 = 128;
const SCALE: u32 = 4;
const CANVAS: u32 = SIZE * SCALE;
const RADIUS: u32 = 28 * SCALE;

fn rounded(x: u32, y: u32, side: u32, radius: u32) -> bool {
    let center = |position| {
        let position = position as f64 + 0.5;
        position - position.clamp(f64::from(radius), f64::from(side - radius))
    };
    center(x).hypot(center(y)) <= f64::from(radius)
}

fn tile(fill: Rgba<u8>) -> RgbaImage {
    let rim = 10;
    let dark = Rgba([
        (f64::from(fill[0]) * 0.62).round() as u8,
        (f64::from(fill[1]) * 0.62).round() as u8,
        (f64::from(fill[2]) * 0.62).round() as u8,
        255,
    ]);
    RgbaImage::from_fn(CANVAS, CANVAS, |x, y| {
        if (rim..CANVAS - rim).contains(&x)
            && (rim..CANVAS - rim).contains(&y)
            && rounded(x - rim, y - rim, CANVAS - 2 * rim, RADIUS - rim)
        {
            fill
        } else if rounded(x, y, CANVAS, RADIUS) {
            dark
        } else {
            Rgba([0; 4])
        }
    })
}

fn font() -> io::Result<Font> {
    let home_font = super::home().join(".local/share/fonts/Inter/Inter.ttc");
    let candidates = [
        (
            Path::new("/usr/share/fonts/truetype/inter/Inter-Black.ttf"),
            0,
        ),
        (
            Path::new("/usr/share/fonts/truetype/inter/Inter-ExtraBold.ttf"),
            0,
        ),
        (
            Path::new("/usr/share/fonts/truetype/inter/Inter-Bold.ttf"),
            0,
        ),
        (home_font.as_path(), 1),
        (
            Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
            0,
        ),
        (
            Path::new("/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"),
            0,
        ),
        (
            Path::new("/usr/share/fonts/truetype/liberation2/LiberationSans-Bold.ttf"),
            0,
        ),
    ];
    for (path, collection_index) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(font) = Font::from_bytes(
                bytes,
                FontSettings {
                    collection_index,
                    ..FontSettings::default()
                },
            ) {
                return Ok(font);
            }
        }
    }
    if let Some(output) = super::simple_cmd(
        &[
            "fc-match",
            "-f",
            "%{file}\n%{index}",
            "sans-serif:style=Bold",
        ],
        2,
    )
    .filter(|output| output.code == Some(0))
    {
        if let Some((path, index)) = output.stdout.split_once('\n') {
            if let Ok(bytes) = std::fs::read(path) {
                let settings = FontSettings {
                    collection_index: index.trim().parse::<u32>().unwrap_or_default() & 0xffff,
                    ..FontSettings::default()
                };
                if let Ok(font) = Font::from_bytes(bytes, settings) {
                    return Ok(font);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No supported badge font is installed",
    ))
}

fn label_mask(font: &Font, label: &str, min_size: u32) -> Option<RgbaImage> {
    let max_width = CANVAS * 78 / 100;
    let max_height = CANVAS * 62 / 100;
    for size in (min_size..=max_height).rev().step_by(2 * SCALE as usize) {
        let mut pen = 0.0_f32;
        let mut previous = None;
        let mut glyphs = Vec::new();
        for ch in label.chars() {
            if let Some(left) = previous {
                pen += font
                    .horizontal_kern(left, ch, size as f32)
                    .unwrap_or_default();
            }
            let metrics = font.metrics(ch, size as f32);
            let x = pen.round() as i32 + metrics.xmin;
            let y = -metrics.ymin - metrics.height as i32;
            glyphs.push((ch, x, y, metrics.width as i32, metrics.height as i32));
            pen += metrics.advance_width;
            previous = Some(ch);
        }
        let left = glyphs.iter().map(|g| g.1).min()?;
        let top = glyphs.iter().map(|g| g.2).min()?;
        let width = (glyphs.iter().map(|g| g.1 + g.3).max()? - left) as u32;
        let height = (glyphs.iter().map(|g| g.2 + g.4).max()? - top) as u32;
        if width > max_width || height > max_height {
            continue;
        }
        let mut mask = RgbaImage::new(width, height);
        for (ch, x, y, w, h) in glyphs {
            let (_, data) = font.rasterize(ch, size as f32);
            for row in 0..h {
                for column in 0..w {
                    let alpha = data[(row * w + column) as usize];
                    mask.put_pixel(
                        (x - left + column) as u32,
                        (y - top + row) as u32,
                        Rgba([255, 255, 255, alpha]),
                    );
                }
            }
        }
        return Some(mask);
    }
    None
}

fn ink(fill: Rgba<u8>) -> [u8; 3] {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(fill[0]) + 0.7152 * linear(fill[1]) + 0.0722 * linear(fill[2]);
    if 1.05 / (luminance + 0.05) >= 4.5 {
        [255; 3]
    } else {
        [20, 20, 22]
    }
}

pub(super) fn write(
    name: &str,
    color: &str,
    custom: &Path,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if !super::profile_ok(name) {
        return Err("Invalid profile name".into());
    }
    let color = color.strip_prefix('#').unwrap_or(color);
    if color.len() != 6 || !color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Expected a six-digit hex color".into());
    }
    let rgb = u32::from_str_radix(color, 16)?.to_be_bytes();
    let fill = Rgba([rgb[1], rgb[2], rgb[3], 255]);
    let mut tile = tile(fill);
    let filter = imageops::FilterType::Lanczos3;
    if custom.is_file() {
        let frame = 6 * SCALE;
        let side = CANVAS - 2 * frame;
        let mut reader = ImageReader::open(custom)?.with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let source = reader.decode()?.into_rgba8();
        let crop = source.width().min(source.height());
        let square = imageops::crop_imm(
            &source,
            (source.width() - crop) / 2,
            (source.height() - crop) / 2,
            crop,
            crop,
        );
        let mut art = imageops::resize(&*square, side, side, filter);
        for (x, y, pixel) in art.enumerate_pixels_mut() {
            if !rounded(x, y, side, RADIUS - frame) {
                pixel[3] = 0;
            }
        }
        imageops::overlay(&mut tile, &art, frame.into(), frame.into());
    } else {
        let font = font()?;
        let short = if name.len() <= 3 {
            name.to_owned()
        } else {
            name[..2].to_ascii_uppercase()
        };
        let mut label = label_mask(&font, name, 40 * SCALE)
            .or_else(|| label_mask(&font, &short, 12 * SCALE))
            .ok_or("Cannot fit the badge label")?;
        let [r, g, b] = ink(fill);
        for pixel in label.pixels_mut() {
            *pixel = Rgba([r, g, b, pixel[3]]);
        }
        imageops::overlay(
            &mut tile,
            &label,
            ((CANVAS - label.width()) / 2).into(),
            ((CANVAS - label.height()) / 2).into(),
        );
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    DynamicImage::ImageRgba8(imageops::resize(&tile, SIZE, SIZE, filter))
        .save_with_format(output, image::ImageFormat::Png)?;
    Ok(())
}
