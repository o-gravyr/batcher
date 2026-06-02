use std::io::BufWriter;
use std::path::Path;
use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, FontVec, PxScale, ScaleFont};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::imageops::FilterType as ResizeFilter;
use image::{ImageEncoder, Rgb, RgbImage, RgbaImage};
use imageproc::drawing::draw_text_mut;
use serde::Deserialize;
use turbojpeg::{Compressor, Image as TjImage, PixelFormat, Subsamp};

const AEONIK_TTF: &[u8] = include_bytes!("../../Data/Aeonik-Regular.ttf");
const LOGO_9X16_SVG: &[u8] = include_bytes!("../../Data/logo_16-9.svg");

const JPEG_QUALITY: i32 = 92;
const TEXT_FONT_PX: f32 = 84.0;
const LINE_HEIGHT_FACTOR: f32 = 1.18;

// Everything is composed on a single 9:16 canvas. The 1:1 output is a centered
// crop of that composed image (no independent square layout). See CLAUDE.md.
const COMP_W: u32 = 1080;
const COMP_H: u32 = 1920;
const COMP_ASPECT: (u32, u32) = (9, 16);
const SQUARE_SIDE: u32 = 1080;
/// template-1 positions, in the 1080×1920 composition's pixel space.
const T1_TEXT_ORIGIN: (i32, i32) = (104, 350);
const T1_LOGO_POS: (i32, i32) = (357, 1297);

/// Output variant. Both derive from the same composed 9:16 canvas: `Portrait916`
/// encodes it as-is; `Square1x1` center-crops it to 1080×1080 first.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Portrait916,
    Square1x1,
}

impl Format {
    pub fn suffix(self) -> &'static str {
        match self {
            Format::Portrait916 => "9x16",
            Format::Square1x1 => "1x1",
        }
    }
}

/// Horizontal anchoring for the text block / logo in `custom` layouts.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    Left,
    Center,
    Right,
}

/// A layout choice coming from the UI. `Template1` reproduces the historical
/// hard-coded positions; `Custom` places the text block and logo from anchors
/// and canvas-relative fractions. Field keys are snake_case to match serde.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutSpec {
    Template1,
    Custom {
        logo_x: Anchor,
        /// Top of the logo, as a fraction (0..1) of canvas height.
        logo_y: f32,
        text_x: Anchor,
        /// Top of the text block, as a fraction (0..1) of canvas height.
        text_y: f32,
        /// Text block width, as a fraction (0..1) of canvas width.
        text_w: f32,
        /// Font size in canvas px (1080-wide space).
        #[serde(default = "default_font_px")]
        font_px: f32,
        /// Line-height multiplier (line advance = font_px × this).
        #[serde(default = "default_line_height")]
        line_height: f32,
        /// Letter-spacing as a percentage of the font size (0 = none).
        #[serde(default)]
        letter_spacing: f32,
    },
}

fn default_font_px() -> f32 {
    TEXT_FONT_PX
}
fn default_line_height() -> f32 {
    LINE_HEIGHT_FACTOR
}

/// A `LayoutSpec` resolved to absolute pixel positions for a given format.
pub struct Layout {
    pub text_origin: (i32, i32),
    pub text_max_width: i32,
    pub logo_pos: (i32, i32),
    pub font_px: f32,
    pub line_height: f32,
    pub letter_spacing_px: f32,
}

/// Left/right margin used for `Left`/`Right` anchored content in custom layouts,
/// matching `template-1`'s text margin so the two modes stay visually consistent.
const CUSTOM_MARGIN: i32 = 104;

fn anchor_x(anchor: Anchor, canvas_w: u32, content_w: u32) -> i32 {
    match anchor {
        Anchor::Left => CUSTOM_MARGIN,
        Anchor::Center => ((canvas_w as i32) - content_w as i32) / 2,
        Anchor::Right => (canvas_w as i32) - content_w as i32 - CUSTOM_MARGIN,
    }
}

/// Resolve a `LayoutSpec` to pixel positions on the 1080×1920 composition.
/// `logo_w`/`logo_h` are the rasterized logo's dimensions (used for `Center`/`Right`
/// anchoring).
pub fn resolve_layout(spec: &LayoutSpec, logo_w: u32, logo_h: u32) -> Layout {
    let (cw, ch) = (COMP_W, COMP_H);
    match spec {
        LayoutSpec::Template1 => {
            let (ox, oy) = T1_TEXT_ORIGIN;
            Layout {
                text_origin: (ox, oy),
                text_max_width: (cw as i32 - 2 * ox).max(0),
                logo_pos: T1_LOGO_POS,
                font_px: TEXT_FONT_PX,
                line_height: LINE_HEIGHT_FACTOR,
                letter_spacing_px: 0.0,
            }
        }
        LayoutSpec::Custom {
            logo_x,
            logo_y,
            text_x,
            text_y,
            text_w,
            font_px,
            line_height,
            letter_spacing,
        } => {
            let text_box_w = (text_w.clamp(0.0, 1.0) * cw as f32).round() as i32;
            let tx = anchor_x(*text_x, cw, text_box_w.max(0) as u32);
            let ty = (text_y.clamp(0.0, 1.0) * ch as f32).round() as i32;
            let lx = anchor_x(*logo_x, cw, logo_w);
            // logo_y is the logo's vertical CENTER → convert to a top-left y.
            let ly = (logo_y.clamp(0.0, 1.0) * ch as f32).round() as i32 - logo_h as i32 / 2;
            let fp = if *font_px > 0.0 { *font_px } else { TEXT_FONT_PX };
            Layout {
                text_origin: (tx, ty),
                text_max_width: text_box_w.max(0),
                logo_pos: (lx, ly),
                font_px: fp,
                line_height: if *line_height > 0.0 { *line_height } else { LINE_HEIGHT_FACTOR },
                letter_spacing_px: letter_spacing / 100.0 * fp,
            }
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

/// Rasterize the brand logo used on the 9:16 composition (the 1:1 output is a
/// crop of that composition, so it shares the same logo).
pub fn rasterize_logo() -> Result<RgbaImage, String> {
    rasterize_svg(LOGO_9X16_SVG, 368, 198)
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
    pub kind: Kind,
}

/// Build the shared 9:16 composition (crop source to 9:16 → resize 1080×1920 →
/// gradient → logo). Text is drawn later, per message, by `compose_text`.
pub fn prepare_canvas(
    source_path: &Path,
    logo: &RgbaImage,
    logo_pos: (i32, i32),
) -> Result<PreparedCanvas, String> {
    let kind = Kind::from_path(source_path).ok_or_else(|| "unsupported format".to_string())?;
    // Decode by sniffing the file's magic bytes, not its extension: some inputs
    // carry the wrong extension (e.g. JPEG bytes in a .png), and extension-based
    // decoding (image::open) would fail on every one of them.
    let img = image::ImageReader::open(source_path)
        .map_err(|e| format!("open: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("read header: {e}"))?
        .decode()
        .map_err(|e| format!("decode: {e}"))?;

    let (w, h) = (img.width(), img.height());
    let (aw, ah) = COMP_ASPECT;
    let cropped = center_crop_to_aspect(img.to_rgb8(), w, h, aw, ah);

    let mut resized = image::imageops::resize(&cropped, COMP_W, COMP_H, ResizeFilter::Lanczos3);

    apply_gradient(&mut resized);

    overlay_rgba(&mut resized, logo, logo_pos.0, logo_pos.1);

    Ok(PreparedCanvas {
        canvas: resized,
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

/// Draw `text` onto a clone of the composed 9:16 canvas and return it. The result
/// is then encoded to one or more `Format`s by `encode_output`.
pub fn compose_text(
    stage: &PreparedCanvas,
    text: &str,
    aeonik: &FontRef<'static>,
    text_origin: (i32, i32),
    text_max_width: i32,
    font_px: f32,
    line_height_factor: f32,
    letter_spacing_px: f32,
) -> RgbImage {
    let mut canvas = stage.canvas.clone();
    let scale = PxScale::from(font_px);
    let line_height = (font_px * line_height_factor).round() as i32;
    let (ox, oy) = text_origin;
    let max_w = text_max_width.max(0) as u32;
    let ls = letter_spacing_px;
    let white = Rgb([255_u8, 255, 255]);

    if font_supports(aeonik, text) {
        draw_multiline(&mut canvas, text, ox, oy, max_w, line_height, scale, white, aeonik, ls);
    } else if let Some(arial) = arial() {
        draw_multiline(&mut canvas, text, ox, oy, max_w, line_height, scale, white, arial, ls);
    } else {
        // No fallback available — render anyway with Aeonik. Missing glyphs
        // become .notdef boxes, but the file is still produced and the user
        // sees the issue plainly.
        draw_multiline(&mut canvas, text, ox, oy, max_w, line_height, scale, white, aeonik, ls);
    }

    canvas
}

/// Encode a composed 9:16 canvas to one `Format`. `Portrait916` writes it as-is;
/// `Square1x1` center-crops it to 1080×1080 first.
pub fn encode_output(
    canvas: &RgbImage,
    format: Format,
    kind: Kind,
    out_path: &Path,
) -> Result<(), String> {
    match format {
        Format::Portrait916 => encode(canvas, kind, out_path),
        Format::Square1x1 => {
            let y0 = (COMP_H - SQUARE_SIDE) / 2;
            let cropped =
                image::imageops::crop_imm(canvas, 0, y0, SQUARE_SIDE, SQUARE_SIDE).to_image();
            encode(&cropped, kind, out_path)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_multiline<F: Font>(
    canvas: &mut RgbImage,
    text: &str,
    ox: i32,
    oy: i32,
    max_w: u32,
    line_height: i32,
    scale: PxScale,
    color: Rgb<u8>,
    font: &F,
    ls_px: f32,
) {
    for (i, line) in text.split('\n').enumerate() {
        let y = oy + (i as i32) * line_height;
        for (j, sub) in wrap_line(line, font, scale, max_w, ls_px)
            .iter()
            .enumerate()
        {
            let yy = y + (j as i32) * line_height;
            if ls_px.abs() < f32::EPSILON {
                // No letter-spacing → draw the whole substring at once (unchanged).
                draw_text_mut(canvas, color, ox, yy, scale, font, sub);
            } else {
                draw_spaced(canvas, sub, ox, yy, scale, color, font, ls_px);
            }
        }
    }
}

/// Draw a single line glyph-by-glyph, adding `ls_px` between glyphs.
fn draw_spaced<F: Font>(
    canvas: &mut RgbImage,
    line: &str,
    ox: i32,
    y: i32,
    scale: PxScale,
    color: Rgb<u8>,
    font: &F,
    ls_px: f32,
) {
    let scaled = font.as_scaled(scale);
    let mut x = ox as f32;
    let mut prev = None;
    let mut buf = [0u8; 4];
    for c in line.chars() {
        let g = font.glyph_id(c);
        if let Some(p) = prev {
            x += scaled.kern(p, g);
        }
        draw_text_mut(canvas, color, x.round() as i32, y, scale, font, c.encode_utf8(&mut buf));
        x += scaled.h_advance(g) + ls_px;
        prev = Some(g);
    }
}

fn wrap_line<F: Font>(line: &str, font: &F, scale: PxScale, max_w: u32, ls_px: f32) -> Vec<String> {
    if max_w == 0 || measure(line, font, scale, ls_px) <= max_w as f32 {
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
        if measure(&tentative, font, scale, ls_px) <= max_w as f32 {
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

fn measure<F: Font>(text: &str, font: &F, scale: PxScale, ls_px: f32) -> f32 {
    let scaled = font.as_scaled(scale);
    let mut width = 0.0_f32;
    let mut last = None;
    for c in text.chars() {
        let g = font.glyph_id(c);
        if let Some(prev) = last {
            width += scaled.kern(prev, g);
        }
        width += scaled.h_advance(g) + ls_px;
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
