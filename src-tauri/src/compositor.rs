use std::io::BufWriter;
use std::path::Path;
use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, FontVec, PxScale, ScaleFont};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::imageops::FilterType as ResizeFilter;
use image::{ImageEncoder, Rgb, RgbImage, RgbaImage};
use imageproc::drawing::draw_text_mut;
use turbojpeg::{Compressor, Image as TjImage, PixelFormat, Subsamp};

const AEONIK_TTF: &[u8] = include_bytes!("../../Data/Aeonik-Regular.ttf");
const LOGO_1X1_SVG: &[u8] = include_bytes!("../../Data/logo_1x1.svg");
const LOGO_9X16_SVG: &[u8] = include_bytes!("../../Data/logo_16-9.svg");

const JPEG_QUALITY: i32 = 92;
const TEXT_FONT_PX: f32 = 84.0;
const LINE_HEIGHT_FACTOR: f32 = 1.18;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Square,
    Portrait916,
}

impl Format {
    pub fn suffix(self) -> &'static str {
        match self {
            Format::Square => "1x1",
            Format::Portrait916 => "9x16",
        }
    }

    fn canvas(self) -> (u32, u32) {
        match self {
            // The Figma frame labelled "1x1" is actually 1080×1350 (4:5).
            // Keep the filename suffix as `_1x1` per spec but match the
            // designer's working canvas pixel-for-pixel.
            Format::Square => (1080, 1350),
            Format::Portrait916 => (1080, 1920),
        }
    }

    fn aspect(self) -> (u32, u32) {
        match self {
            Format::Square => (4, 5),
            Format::Portrait916 => (9, 16),
        }
    }

    fn text_origin(self) -> (i32, i32) {
        match self {
            Format::Square => (104, 260),
            Format::Portrait916 => (104, 350),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Png,
}

impl Kind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" => Some(Kind::Jpeg),
            "png" => Some(Kind::Png),
            _ => None,
        }
    }
}

pub struct LogoSet {
    pub square: RgbaImage,
    pub portrait: RgbaImage,
}

pub fn rasterize_logos() -> Result<LogoSet, String> {
    Ok(LogoSet {
        square: rasterize_svg(LOGO_1X1_SVG, 268, 144)?,
        portrait: rasterize_svg(LOGO_9X16_SVG, 368, 198)?,
    })
}

fn rasterize_svg(svg_bytes: &[u8], w: u32, h: u32) -> Result<RgbaImage, String> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opt).map_err(|e| format!("svg parse: {e}"))?;
    let tree_size = tree.size();
    let sx = w as f32 / tree_size.width();
    let sy = h as f32 / tree_size.height();
    let scale = sx.min(sy);
    let mut pixmap =
        tiny_skia::Pixmap::new(w, h).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny_skia returns premultiplied RGBA — convert to straight RGBA for compositing.
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let a = px.alpha();
        if a == 0 {
            data.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let r = ((px.red() as u16 * 255 + (a as u16) / 2) / a as u16) as u8;
            let g = ((px.green() as u16 * 255 + (a as u16) / 2) / a as u16) as u8;
            let b = ((px.blue() as u16 * 255 + (a as u16) / 2) / a as u16) as u8;
            data.extend_from_slice(&[r, g, b, a]);
        }
    }
    RgbaImage::from_raw(w, h, data).ok_or_else(|| "logo image alloc failed".to_string())
}

pub struct PreparedCanvas {
    pub canvas: RgbImage,
    pub format: Format,
    pub kind: Kind,
}

pub fn prepare_canvas(
    source_path: &Path,
    format: Format,
    logos: &LogoSet,
) -> Result<PreparedCanvas, String> {
    let kind = Kind::from_path(source_path).ok_or_else(|| "unsupported format".to_string())?;
    let img = image::open(source_path).map_err(|e| format!("decode: {e}"))?;

    let (w, h) = (img.width(), img.height());
    let (aw, ah) = format.aspect();
    let cropped = center_crop_to_aspect(img.to_rgb8(), w, h, aw, ah);

    let (cw, ch) = format.canvas();
    let mut resized = image::imageops::resize(&cropped, cw, ch, ResizeFilter::Lanczos3);

    apply_gradient(&mut resized);

    let logo = match format {
        Format::Square => &logos.square,
        Format::Portrait916 => &logos.portrait,
    };
    let (logo_x, logo_y) = match format {
        Format::Square => (((cw as i32) - (logo.width() as i32)) / 2, 931),
        Format::Portrait916 => (357, 1297),
    };
    overlay_rgba(&mut resized, logo, logo_x, logo_y);

    Ok(PreparedCanvas {
        canvas: resized,
        format,
        kind,
    })
}

fn center_crop_to_aspect(rgb: RgbImage, w: u32, h: u32, aw: u32, ah: u32) -> RgbImage {
    // Find the largest centered rectangle of aspect aw:ah inside w×h.
    let target_w_from_h = (h as u64 * aw as u64 / ah as u64) as u32;
    let target_h_from_w = (w as u64 * ah as u64 / aw as u64) as u32;

    let (cx, cy, cw, ch) = if target_w_from_h <= w {
        // Pillarbox the height — full height, narrower width
        let cw = target_w_from_h;
        let cx = (w - cw) / 2;
        (cx, 0, cw, h)
    } else {
        // Letterbox the width — full width, shorter height
        let ch = target_h_from_w;
        let cy = (h - ch) / 2;
        (0, cy, w, ch)
    };
    image::imageops::crop_imm(&rgb, cx, cy, cw, ch).to_image()
}

fn apply_gradient(img: &mut RgbImage) {
    let h = img.height() as f32;
    let cutoff = 0.40183_f32;
    let cutoff_y = h * cutoff;
    // RGB color of the overlay (rgba(14,14,12,0.9) at the top of the gradient).
    let fr = 14.0_f32;
    let fg = 14.0_f32;
    let fb = 12.0_f32;
    let max_alpha = 0.9_f32;

    let height = img.height();
    let width = img.width();
    for y in 0..height {
        let yf = y as f32;
        if yf >= cutoff_y {
            break;
        }
        let alpha = max_alpha * (1.0 - yf / cutoff_y);
        let inv = 1.0 - alpha;
        for x in 0..width {
            let p = img.get_pixel_mut(x, y);
            p[0] = (p[0] as f32 * inv + fr * alpha).round().clamp(0.0, 255.0) as u8;
            p[1] = (p[1] as f32 * inv + fg * alpha).round().clamp(0.0, 255.0) as u8;
            p[2] = (p[2] as f32 * inv + fb * alpha).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn overlay_rgba(canvas: &mut RgbImage, fg: &RgbaImage, x: i32, y: i32) {
    let cw = canvas.width() as i32;
    let ch = canvas.height() as i32;
    for fy in 0..fg.height() as i32 {
        let dy = y + fy;
        if dy < 0 || dy >= ch {
            continue;
        }
        for fx in 0..fg.width() as i32 {
            let dx = x + fx;
            if dx < 0 || dx >= cw {
                continue;
            }
            let p = fg.get_pixel(fx as u32, fy as u32).0;
            let a = p[3] as f32 / 255.0;
            if a <= 0.0 {
                continue;
            }
            let inv = 1.0 - a;
            let dst = canvas.get_pixel_mut(dx as u32, dy as u32);
            dst[0] = ((dst[0] as f32) * inv + (p[0] as f32) * a)
                .round()
                .clamp(0.0, 255.0) as u8;
            dst[1] = ((dst[1] as f32) * inv + (p[1] as f32) * a)
                .round()
                .clamp(0.0, 255.0) as u8;
            dst[2] = ((dst[2] as f32) * inv + (p[2] as f32) * a)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

pub fn aeonik() -> Result<FontRef<'static>, String> {
    FontRef::try_from_slice(AEONIK_TTF).map_err(|e| format!("load Aeonik: {e}"))
}

fn arial() -> Option<&'static FontVec> {
    static ARIAL: OnceLock<Option<FontVec>> = OnceLock::new();
    ARIAL
        .get_or_init(|| {
            let candidates: Vec<&str> = if cfg!(target_os = "macos") {
                vec![
                    "/System/Library/Fonts/Supplemental/Arial.ttf",
                    "/Library/Fonts/Arial.ttf",
                    "/System/Library/Fonts/Helvetica.ttc",
                ]
            } else if cfg!(target_os = "windows") {
                vec!["C:\\Windows\\Fonts\\arial.ttf"]
            } else {
                vec![]
            };
            for path in candidates {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = FontVec::try_from_vec(bytes) {
                        return Some(font);
                    }
                }
            }
            None
        })
        .as_ref()
}

fn font_supports(font: &impl Font, text: &str) -> bool {
    text.chars().all(|c| {
        if c.is_whitespace() || c == '\n' {
            return true;
        }
        font.glyph_id(c).0 != 0
    })
}

pub fn render_with_text(
    stage: &PreparedCanvas,
    text: &str,
    aeonik: &FontRef<'static>,
    out_path: &Path,
) -> Result<(), String> {
    let mut canvas = stage.canvas.clone();
    let scale = PxScale::from(TEXT_FONT_PX);
    let line_height = (TEXT_FONT_PX * LINE_HEIGHT_FACTOR).round() as i32;
    let (ox, oy) = stage.format.text_origin();
    let white = Rgb([255_u8, 255, 255]);

    if font_supports(aeonik, text) {
        draw_multiline(&mut canvas, text, ox, oy, line_height, scale, white, aeonik);
    } else if let Some(arial) = arial() {
        draw_multiline(&mut canvas, text, ox, oy, line_height, scale, white, arial);
    } else {
        // No fallback available — render anyway with Aeonik. Missing glyphs
        // become .notdef boxes, but the file is still produced and the user
        // sees the issue plainly.
        draw_multiline(&mut canvas, text, ox, oy, line_height, scale, white, aeonik);
    }

    encode(&canvas, stage.kind, out_path)
}

fn draw_multiline<F: Font>(
    canvas: &mut RgbImage,
    text: &str,
    ox: i32,
    oy: i32,
    line_height: i32,
    scale: PxScale,
    color: Rgb<u8>,
    font: &F,
) {
    for (i, line) in text.split('\n').enumerate() {
        let y = oy + (i as i32) * line_height;
        // Wrap at canvas width minus matching right margin (= ox).
        let max_w = canvas.width() as i32 - 2 * ox;
        for (j, sub) in wrap_line(line, font, scale, max_w.max(0) as u32)
            .iter()
            .enumerate()
        {
            let yy = y + (j as i32) * line_height;
            draw_text_mut(canvas, color, ox, yy, scale, font, sub);
        }
    }
}

fn wrap_line<F: Font>(line: &str, font: &F, scale: PxScale, max_w: u32) -> Vec<String> {
    if max_w == 0 || measure(line, font, scale) <= max_w as f32 {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in line.split_whitespace() {
        let tentative = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if measure(&tentative, font, scale) <= max_w as f32 {
            cur = tentative;
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn measure<F: Font>(text: &str, font: &F, scale: PxScale) -> f32 {
    let scaled = font.as_scaled(scale);
    let mut width = 0.0_f32;
    let mut last = None;
    for c in text.chars() {
        let g = font.glyph_id(c);
        if let Some(prev) = last {
            width += scaled.kern(prev, g);
        }
        width += scaled.h_advance(g);
        last = Some(g);
    }
    width
}

fn encode(canvas: &RgbImage, kind: Kind, out_path: &Path) -> Result<(), String> {
    match kind {
        Kind::Jpeg => encode_jpeg(canvas, out_path),
        Kind::Png => encode_png(canvas, out_path),
    }
}

fn encode_jpeg(canvas: &RgbImage, out_path: &Path) -> Result<(), String> {
    let w = canvas.width() as usize;
    let h = canvas.height() as usize;
    let mut compressor = Compressor::new().map_err(|e| format!("compressor init: {e}"))?;
    compressor
        .set_quality(JPEG_QUALITY)
        .map_err(|e| format!("set quality: {e}"))?;
    compressor
        .set_subsamp(Subsamp::Sub2x2)
        .map_err(|e| format!("set subsamp: {e}"))?;

    let raw = canvas.as_raw();
    let img_in = TjImage {
        pixels: raw.as_slice(),
        width: w,
        pitch: w * 3,
        height: h,
        format: PixelFormat::RGB,
    };
    let jpeg = compressor
        .compress_to_vec(img_in)
        .map_err(|e| format!("JPEG encode: {e}"))?;
    std::fs::write(out_path, &jpeg).map_err(|e| format!("write: {e}"))?;
    Ok(())
}

fn encode_png(canvas: &RgbImage, out_path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out_path).map_err(|e| format!("create file: {e}"))?;
    let writer = BufWriter::new(file);
    let encoder = PngEncoder::new_with_quality(writer, CompressionType::Fast, FilterType::Adaptive);
    encoder
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(())
}
