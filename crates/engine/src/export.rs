//! Export a PDF ▸ Image and Text (execution plan M10.3, the first formats).
//!
//! [`Exporter`] renders pages to PNG, JPEG or TIFF at a resolution and extracts reading-order
//! text, from a
//! document's working file (so unsaved edits and hidden layers are respected, as on screen).

use asdreader_render::{MAX_PIXELS, PageRenderer, RenderRequest, RequestKind, Tile, device_pixels, effective_scale};

use crate::Document;

/// Renders and extracts pages of one document state.
pub struct Exporter {
    renderer: PageRenderer,
    pages: usize,
    /// Each page's displayed size in points, for [`Exporter::dpi_used`].
    sizes: Vec<(f32, f32)>,
    /// Pages are drawn on a transparent background (`RenderConfig::transparent`).
    transparent: bool,
    /// Most pixels in one band of a page drawn in bands ([`BAND_PIXELS`]; smaller in tests).
    pub(crate) band_pixels: f64,
}

/// Most pixels on a side of an exported JPEG: the format's limit.
pub const JPEG_MAX_SIDE: f64 = 65_500.0;
/// Most pixels on a side of an exported PNG or TIFF (8.5 m at 600 dpi).
pub const EXPORT_MAX_SIDE: f64 = 200_000.0;
/// Most pixels in an exported image. A page larger than the renderer draws at once is drawn in
/// bands and every format is written a band at a time, so this bounds the file and the time, not
/// the memory: an A0L2 sheet (841 × 2378 mm) at 600 dpi is 1,116 million pixels.
pub const EXPORT_MAX_PIXELS: f64 = 4.0e9;
/// The rows of one band are at most this many pixels (the renderer's own limit for one picture).
const BAND_PIXELS: f64 = MAX_PIXELS as f64;
/// The widest piece of a band the renderer draws at once (its sizes are u16).
const BAND_TILE: u32 = 32_768;

/// What an export needs from a document, as plain values that can move to a worker thread.
#[derive(Clone)]
pub struct ExportSource {
    pub bytes: std::sync::Arc<Vec<u8>>,
    pub config: asdreader_render::RenderConfig,
    pub pages: usize,
    /// Each page's displayed size in points (after `/Rotate` and `/UserUnit`).
    pub sizes: Vec<(f32, f32)>,
}

impl Document {
    /// What an export reads, for exporting on another thread. These are the displayed
    /// bytes, so they may carry missing comment appearances ASDReader drew. Signature
    /// artwork stays original; viewer validation badges are not document content and
    /// do not enter image/text exports or Print as Image. Saving uses the working file.
    pub fn export_source(&self) -> ExportSource {
        ExportSource {
            bytes: self.display.clone(),
            config: self.config.clone(),
            pages: self.info.pages.len(),
            sizes: self.info.pages.iter().map(|p| (p.width, p.height)).collect(),
        }
    }
}

/// Export all images: the images `pages` (0-based) use, each once, skipping those under
/// `min_side` pixels on their shorter side. JPEGs come out unchanged, other images as PNG.
pub fn extract_images(src: &ExportSource, pages: &[usize], min_side: u32) -> Result<asdreader_create::ImageExport, String> {
    let doc = asdreader_cos::Document::open_with_password(src.bytes.clone(), src.config.password.as_deref()).map_err(|e| e.to_string())?;
    Ok(asdreader_create::extract_images(&doc, pages, min_side))
}

/// The file name for the `index`-th (1-based) exported image: `<stem>_Page_<n>_Image_<index>.<ext>`.
/// The page number has leading zeros for as many digits as the document's `page_count` has, so the
/// files sort in page order (#2208): page 7 of 120 is `Page_007`.
pub fn image_file_name(stem: &str, image: &asdreader_create::ExtractedImage, index: usize, page_count: usize) -> String {
    let width = page_count.max(1).to_string().len();
    format!("{stem}_Page_{:0width$}_Image_{index:04}.{}", image.page + 1, image.extension)
}

impl Exporter {
    pub fn new(doc: &Document) -> Self {
        Self::from_source(doc.export_source())
    }

    /// An exporter that draws a page too large for the renderer's size limits at the largest
    /// resolution they allow, as the interactive Export does; [`Exporter::dpi_used`] says at what.
    /// A source whose `config.reject_oversize` is set refuses such pages instead.
    pub fn from_source(src: ExportSource) -> Self {
        let transparent = src.config.transparent;
        Self { renderer: PageRenderer::new(src.bytes, src.config), pages: src.pages, sizes: src.sizes, transparent, band_pixels: BAND_PIXELS }
    }

    /// The resolution page `page` is exported at as an image of `format` when asked for `dpi`:
    /// as asked, unless the image would be over [`EXPORT_MAX_PIXELS`] or longer on a side than
    /// the format allows ([`JPEG_MAX_SIDE`], [`EXPORT_MAX_SIDE`]). Pages larger than the renderer
    /// draws at once are drawn in bands, so its own limits don't apply.
    pub fn image_dpi(&self, page: usize, dpi: f64, format: ImageFormat) -> f64 {
        let dpi = dpi.clamp(18.0, 1200.0);
        let Some(&(w, h)) = self.sizes.get(page) else { return dpi };
        let (w, h) = (f64::from(w.max(1.0)), f64::from(h.max(1.0)));
        let side = if matches!(format, ImageFormat::Jpeg { .. }) { JPEG_MAX_SIDE } else { EXPORT_MAX_SIDE };
        let by_side = side / w.max(h) * 72.0;
        let by_area = (EXPORT_MAX_PIXELS / (w * h)).sqrt() * 72.0;
        // One pixel of slack: `device_pixels` rounds up.
        dpi.min(by_side * (1.0 - 1e-6)).min(by_area * (1.0 - 1e-6))
    }

    /// The resolution page `page` is actually exported at when asked for `dpi`: lower than asked
    /// when the page is too large for the renderer's size limits at that resolution.
    pub fn dpi_used(&self, page: usize, dpi: f64) -> f64 {
        let dpi = dpi.clamp(18.0, 1200.0);
        match self.sizes.get(page) {
            Some(&(w, h)) => f64::from(asdreader_render::effective_scale(w, h, (dpi / 72.0) as f32)) * 72.0,
            None => dpi,
        }
    }

    fn check(&self, page: usize) -> Result<(), String> {
        if page < self.pages { Ok(()) } else { Err(format!("page {} does not exist", page + 1)) }
    }

    /// Page `page` (0-based) as a PNG at `dpi`, at most the renderer's size limits allow (see
    /// [`Exporter::dpi_used`]); refused instead when the source asked for `reject_oversize`.
    pub fn png(&mut self, page: usize, dpi: f64) -> Result<Vec<u8>, String> {
        self.check(page)?;
        let r = self.renderer.render(RenderRequest { page, scale: (dpi.clamp(18.0, 1200.0) / 72.0) as f32, ..Default::default() });
        if let Some(e) = r.error {
            return Err(format!("page {}: {e}", page + 1));
        }
        encode_png(r.width, r.height, &r.rgba)
    }

    /// Page `page` as an image file of `format` at `dpi` (see [`Exporter::image_dpi`]), which says
    /// the resolution it was drawn at. A page larger than the renderer draws at once (an
    /// engineering sheet at a few hundred dpi) is drawn in bands as wide as the page and joined.
    pub fn image(&mut self, page: usize, dpi: f64, format: ImageFormat) -> Result<Vec<u8>, String> {
        self.check(page)?;
        let dpi = self.image_dpi(page, dpi, format);
        let scale = (dpi / 72.0) as f32;
        let (w_pt, h_pt) = self.sizes.get(page).copied().unwrap_or((612.0, 792.0));
        if effective_scale(w_pt, h_pt, scale) >= scale && self.band_pixels >= BAND_PIXELS {
            let r = self.renderer.render(RenderRequest { page, scale, ..Default::default() });
            if let Some(e) = r.error {
                return Err(format!("page {}: {e}", page + 1));
            }
            return encode_image(r.width, r.height, &r.rgba, format, dpi);
        }
        let (width, height) = (device_pixels(w_pt, scale), device_pixels(h_pt, scale));
        // Whole MCU rows (8) per band, so a JPEG block never spans two bands.
        let band_rows = (((self.band_pixels.min(BAND_PIXELS) / f64::from(width)) as u32) / 8 * 8).clamp(8, height.next_multiple_of(8));
        let renderer = &mut self.renderer;
        let band = |y: u32| -> Result<Vec<u8>, String> { render_band(renderer, page, scale, width, y, band_rows.min(height - y)) };
        match format {
            ImageFormat::Png => encode_png_bands(width, height, dpi, self.transparent, (0..height).step_by(band_rows as usize).map(band)),
            ImageFormat::Tiff => {
                let rgb = (0..height).step_by(band_rows as usize).map(band).map(|b| b.map(|b| over_white(&b)));
                encode_tiff_bands(width, height, dpi, rgb)
            }
            ImageFormat::Jpeg { quality } => encode_jpeg_bands(width, height, band_rows, quality, dpi, band),
        }
    }

    /// The reading-order text of a page.
    pub fn text(&mut self, page: usize) -> Result<String, String> {
        self.check(page)?;
        let r = self.renderer.render(RenderRequest { page, kind: RequestKind::Text, scale: 1.0, ..Default::default() });
        match r.text {
            Some(t) => Ok(t.plain_text()),
            None => Err(format!("page {}: {}", page + 1, r.error.unwrap_or_else(|| "no text layer".into()))),
        }
    }

    /// The text of several pages, separated by form feeds (as `pdftotext` does).
    pub fn text_of(&mut self, pages: &[usize]) -> Result<String, String> {
        let mut out = String::new();
        for (k, p) in pages.iter().enumerate() {
            if k > 0 {
                out.push('\u{c}');
            }
            out.push_str(self.text(*p)?.trim_end());
            out.push('\n');
        }
        Ok(out)
    }
}

/// Premultiplied RGBA → an image file of `format`. A JPEG or TIFF says it is `dpi` dots per inch,
/// so other programs show and print it at the page's size. A PNG says nothing: its `pHYs` counts
/// pixels per metre, so 72 dpi would read back as 72.009 and a page made from it would shrink.
pub fn encode_image(width: u32, height: u32, premultiplied: &[u8], format: ImageFormat, dpi: f64) -> Result<Vec<u8>, String> {
    match format {
        ImageFormat::Png => encode_png_at(width, height, premultiplied, Some(dpi)),
        ImageFormat::Jpeg { quality } => encode_jpeg(width, height, premultiplied, quality, dpi),
        ImageFormat::Tiff => encode_tiff(width, height, premultiplied, dpi),
    }
}

/// Premultiplied RGBA → PNG (straight alpha). A raster with no translucent pixel (a rendered
/// page always is one: it is drawn on white) is written as RGB.
///
/// Deflate level 3 with adaptive filtering: on rendered pages it writes files the size of the
/// `png` crate's default (level 6) in a third to a fifth of the time. Measured with RGB at
/// level 3 against RGBA at the default: a 300-dpi scan drawn at 220 dpi took 805 → 167 ms
/// (4.46 → 4.47 MB), a text page at 220 dpi 70 → 40 ms (0.85 → 0.89 MB), and an annual
/// report page at 150 dpi 179 → 64 ms (1.41 → 1.38 MB).
pub fn encode_png(width: u32, height: u32, premultiplied: &[u8]) -> Result<Vec<u8>, String> {
    encode_png_at(width, height, premultiplied, None)
}

/// [`encode_png`], saying it is `dpi` dots per inch when given (`pHYs`, in pixels per metre).
pub fn encode_png_at(width: u32, height: u32, premultiplied: &[u8], dpi: Option<f64>) -> Result<Vec<u8>, String> {
    check_raster(width, height, premultiplied)?;
    let pixels = premultiplied.as_chunks::<4>().0;
    let opaque = pixels.iter().all(|px| px[3] == 255);
    let (color, data) = if opaque {
        (png::ColorType::Rgb, pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect::<Vec<u8>>())
    } else {
        let mut rgba = premultiplied.to_vec();
        unpremultiply(&mut rgba);
        (png::ColorType::Rgba, rgba)
    };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(color);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_deflate_compression(png::DeflateCompression::Level(3));
    enc.set_filter(png::Filter::Adaptive);
    enc.set_pixel_dims(dpi.map(png_density));
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&data).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// `dpi` as a PNG's `pHYs`: whole pixels per metre. 300 dpi is 11,811 per metre (299.9994 dpi);
/// ASDReader reads a value that rounds back to a whole number of dpi as that number.
fn png_density(dpi: f64) -> png::PixelDimensions {
    let ppm = (dpi / 0.0254).round().clamp(1.0, f64::from(u32::MAX)) as u32;
    png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter }
}

/// Premultiplied RGBA → straight RGBA, in place.
fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

/// A page drawn in bands (premultiplied RGBA, top to bottom, `width` wide) → PNG at `dpi`,
/// written a band at a time: RGB, or straight RGBA when the page was drawn `transparent`.
fn encode_png_bands(
    width: u32,
    height: u32,
    dpi: f64,
    transparent: bool,
    bands: impl Iterator<Item = Result<Vec<u8>, String>>,
) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let err = |e: png::EncodingError| e.to_string();
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(if transparent { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_depth(png::BitDepth::Eight);
    enc.set_deflate_compression(png::DeflateCompression::Level(3));
    enc.set_filter(png::Filter::Adaptive);
    enc.set_pixel_dims(Some(png_density(dpi)));
    let mut writer = enc.write_header().map_err(err)?;
    let mut stream = writer.stream_writer().map_err(err)?;
    let mut rows = 0u64;
    for band in bands {
        let mut band = band?;
        check_raster(width, (band.len() / 4 / width as usize) as u32, &band)?;
        rows += (band.len() / 4 / width as usize) as u64;
        let data = if transparent {
            unpremultiply(&mut band);
            band
        } else {
            band.as_chunks::<4>().0.iter().flat_map(|px| [px[0], px[1], px[2]]).collect()
        };
        stream.write_all(&data).map_err(|e| e.to_string())?;
    }
    if rows != u64::from(height) {
        return Err(format!("the bands came to {rows} rows, not {height}"));
    }
    stream.finish().map_err(err)?;
    drop(writer);
    Ok(out)
}

/// The resolution tags of a TIFF, in hundredths of a dpi: a page drawn at a lowered
/// resolution (`Exporter::image_dpi`) is rarely a whole number.
fn tiff_resolution<W: std::io::Write + std::io::Seek, C: tiff::encoder::colortype::ColorType, K: tiff::encoder::TiffKind>(
    image: &mut tiff::encoder::ImageEncoder<'_, W, C, K>,
    dpi: f64,
) -> tiff::TiffResult<()> {
    use tiff::encoder::Rational;
    use tiff::tags::{ResolutionUnit, Tag};
    let resolution = Rational { n: (dpi * 100.0).round() as u32, d: 100 };
    let tags = image.encoder();
    tags.write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch)?;
    tags.write_tag(Tag::XResolution, resolution.clone())?;
    tags.write_tag(Tag::YResolution, resolution)
}

/// Rows `y..y + rows` of page `page` at `scale`, `width` pixels wide, as premultiplied RGBA: drawn
/// in pieces at most [`BAND_TILE`] wide and joined.
fn render_band(renderer: &mut PageRenderer, page: usize, scale: f32, width: u32, y: u32, rows: u32) -> Result<Vec<u8>, String> {
    let row_bytes = width as usize * 4;
    let mut band = Vec::new();
    band.try_reserve_exact(row_bytes * rows as usize).map_err(|_| "not enough memory for a band of the image".to_string())?;
    band.resize(row_bytes * rows as usize, 0);
    for x in (0..width).step_by(BAND_TILE as usize) {
        let tile = Tile { x, y, w: BAND_TILE.min(width - x), h: rows };
        let r = renderer.render(RenderRequest { page, scale, tile: Some(tile), ..Default::default() });
        if let Some(e) = r.error {
            return Err(format!("page {}: {e}", page + 1));
        }
        if (r.width, r.height) != (tile.w, tile.h) || r.rgba.len() != tile.w as usize * tile.h as usize * 4 {
            return Err(format!("page {}: a piece came out {}×{}, not {}×{}", page + 1, r.width, r.height, tile.w, tile.h));
        }
        let piece = tile.w as usize * 4;
        for (row, src) in r.rgba.chunks_exact(piece).enumerate() {
            let at = row * row_bytes + x as usize * 4;
            band[at..at + piece].copy_from_slice(src);
        }
    }
    Ok(band)
}

/// Bands of RGB rows (top to bottom, `width` wide) → a baseline TIFF (RGB, LZW, strips of about
/// 64 KB) at `dpi`, written here: the `tiff` crate compresses only an image written whole.
fn encode_tiff_bands(width: u32, height: u32, dpi: f64, bands: impl Iterator<Item = Result<Vec<u8>, String>>) -> Result<Vec<u8>, String> {
    let row_bytes = width as usize * 3;
    let rows_per_strip = (65_536 / row_bytes).clamp(1, height as usize);
    let mut out = b"II*\0\0\0\0\0".to_vec();
    let (mut offsets, mut counts) = (Vec::new(), Vec::new());
    let mut strip = Vec::with_capacity(rows_per_strip * row_bytes);
    let mut rows = 0usize;
    let mut flush = |strip: &mut Vec<u8>, out: &mut Vec<u8>| -> Result<(), String> {
        let packed = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(strip).map_err(|e| e.to_string())?;
        offsets.push(out.len());
        counts.push(packed.len());
        out.extend_from_slice(&packed);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        strip.clear();
        Ok(())
    };
    for band in bands {
        let band = band?;
        if band.len() % row_bytes != 0 {
            return Err(format!("a band of {} bytes is not whole rows of {row_bytes}", band.len()));
        }
        for row in band.chunks_exact(row_bytes) {
            strip.extend_from_slice(row);
            rows += 1;
            if strip.len() == rows_per_strip * row_bytes {
                flush(&mut strip, &mut out)?;
            }
        }
    }
    if !strip.is_empty() {
        flush(&mut strip, &mut out)?;
    }
    if rows != height as usize {
        return Err(format!("the bands came to {rows} rows, not {height}"));
    }
    // The directory, after the strips; its arrays and rationals after it.
    let long = |v: usize| u32::try_from(v).map_err(|_| "the TIFF would be over 4 GB; choose PNG or a lower resolution".to_string());
    let resolution = [(dpi * 100.0).round() as u32, 100];
    let n_strips = offsets.len();
    let ifd = out.len();
    let entries = 13usize;
    let extra = ifd + 2 + entries * 12 + 4;
    let bits_at = extra;
    let xres_at = bits_at + 6 + 2;
    let yres_at = xres_at + 8;
    let offsets_at = yres_at + 8;
    let counts_at = offsets_at + 4 * n_strips;
    let array = |at: usize, n: usize, inline: u32| -> Result<u32, String> { if n == 1 { Ok(inline) } else { long(at) } };
    let mut dir: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, width),
        (257, 4, 1, height),
        (258, 3, 3, long(bits_at)?),
        (259, 3, 1, 5),
        (262, 3, 1, 2),
        (273, 4, n_strips as u32, array(offsets_at, n_strips, long(offsets.first().copied().unwrap_or(0))?)?),
        (277, 3, 1, 3),
        (278, 4, 1, rows_per_strip as u32),
        (279, 4, n_strips as u32, array(counts_at, n_strips, long(counts.first().copied().unwrap_or(0))?)?),
        (282, 5, 1, long(xres_at)?),
        (283, 5, 1, long(yres_at)?),
        (284, 3, 1, 1),
        (296, 3, 1, 2),
    ];
    dir.sort_by_key(|e| e.0);
    out.extend_from_slice(&(entries as u16).to_le_bytes());
    for (tag, kind, count, value) in dir {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        // A single SHORT sits in the low half of the value field.
        if kind == 3 && count == 1 {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&[8, 0, 8, 0, 8, 0, 0, 0]);
    for r in [resolution, resolution] {
        out.extend_from_slice(&r[0].to_le_bytes());
        out.extend_from_slice(&r[1].to_le_bytes());
    }
    if n_strips > 1 {
        for o in &offsets {
            out.extend_from_slice(&long(*o)?.to_le_bytes());
        }
        for c in &counts {
            out.extend_from_slice(&long(*c)?.to_le_bytes());
        }
    }
    long(out.len())?;
    out[4..8].copy_from_slice(&long(ifd)?.to_le_bytes());
    Ok(out)
}

/// A page drawn in bands of `band_rows` rows (a multiple of 8), read by the JPEG encoder as it
/// goes: it takes the image a row of 8 × 8 blocks at a time, top to bottom.
struct Banded<F> {
    width: u32,
    height: u32,
    band_rows: u32,
    state: std::cell::RefCell<BandState<F>>,
}

struct BandState<F> {
    draw: F,
    top: Option<u32>,
    rgb: Vec<u8>,
    error: Option<String>,
}

impl<F: FnMut(u32) -> Result<Vec<u8>, String>> image::GenericImageView for Banded<F> {
    type Pixel = image::Rgb<u8>;

    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn get_pixel(&self, x: u32, y: u32) -> image::Rgb<u8> {
        const PAPER: image::Rgb<u8> = image::Rgb([255, 255, 255]);
        let Ok(mut s) = self.state.try_borrow_mut() else { return PAPER };
        let top = y / self.band_rows * self.band_rows;
        if s.top != Some(top) && s.error.is_none() {
            s.top = Some(top);
            match (s.draw)(top) {
                Ok(band) => s.rgb = over_white(&band),
                Err(e) => s.error = Some(e),
            }
        }
        let at = ((y - top) as usize * self.width as usize + x as usize) * 3;
        match s.rgb.get(at..at + 3) {
            Some(px) => image::Rgb([px[0], px[1], px[2]]),
            None => PAPER,
        }
    }
}

/// A page drawn in bands → baseline JPEG at `dpi`, encoded as the bands are drawn.
fn encode_jpeg_bands(
    width: u32,
    height: u32,
    band_rows: u32,
    quality: u8,
    dpi: f64,
    draw: impl FnMut(u32) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::{JpegEncoder, PixelDensity};
    let source = Banded { width, height, band_rows, state: std::cell::RefCell::new(BandState { draw, top: None, rgb: Vec::new(), error: None }) };
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    enc.set_pixel_density(PixelDensity::dpi(dpi.round() as u16));
    enc.encode_image(&source).map_err(|e| e.to_string())?;
    if let Some(e) = source.state.into_inner().error {
        return Err(e);
    }
    Ok(out)
}

/// Image file formats for Export ▸ Image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    /// Quality 1–100.
    Jpeg {
        quality: u8,
    },
    Tiff,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg { .. } => "jpg",
            ImageFormat::Tiff => "tif",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg { .. } => "JPEG",
            ImageFormat::Tiff => "TIFF",
        }
    }
}

/// Checks that `premultiplied` is exactly `width × height` RGBA pixels, and at least one,
/// before any encoder sees it: `as_chunks` drops a partial trailing pixel, so the rest would be
/// written as a valid image, and the JPEG and TIFF encoders panic on a buffer that doesn't fit
/// the size.
fn check_raster(width: u32, height: u32, premultiplied: &[u8]) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("a {width}×{height} raster has no pixels"));
    }
    let expected = usize::try_from(width).ok().zip(usize::try_from(height).ok()).and_then(|(w, h)| w.checked_mul(h)?.checked_mul(4));
    match expected {
        Some(n) if n == premultiplied.len() => Ok(()),
        Some(n) => Err(format!("a {width}×{height} raster is {n} bytes of RGBA, got {}", premultiplied.len())),
        None => Err(format!("a {width}×{height} raster is too large")),
    }
}

/// Premultiplied RGBA composited over white → RGB.
fn over_white(premultiplied: &[u8]) -> Vec<u8> {
    premultiplied
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let k = 255 - p[3];
            [p[0].saturating_add(k), p[1].saturating_add(k), p[2].saturating_add(k)]
        })
        .collect()
}

/// Premultiplied RGBA → baseline JPEG at `dpi` (pages are opaque: transparency becomes white paper).
pub fn encode_jpeg(width: u32, height: u32, premultiplied: &[u8], quality: u8, dpi: f64) -> Result<Vec<u8>, String> {
    check_raster(width, height, premultiplied)?;
    encode_jpeg_rgb(width, height, &over_white(premultiplied), quality, dpi)
}

/// RGB → baseline JPEG at `dpi`.
fn encode_jpeg_rgb(width: u32, height: u32, rgb: &[u8], quality: u8, dpi: f64) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    use image::codecs::jpeg::{JpegEncoder, PixelDensity};
    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100));
    // The JFIF density is whole dots per inch.
    enc.set_pixel_density(PixelDensity::dpi(dpi.round() as u16));
    enc.write_image(rgb, width, height, image::ExtendedColorType::Rgb8).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Premultiplied RGBA → TIFF (RGB, LZW-compressed) at `dpi`. Written with `tiff` directly:
/// `image`'s `TiffEncoder` always says 1 × 1 with no unit and leaves the strips uncompressed.
/// No predictor: LZW alone is the compression TIFF readers support most widely after none.
pub fn encode_tiff(width: u32, height: u32, premultiplied: &[u8], dpi: f64) -> Result<Vec<u8>, String> {
    check_raster(width, height, premultiplied)?;
    encode_tiff_rgb(width, height, &over_white(premultiplied), dpi)
}

/// RGB → TIFF (LZW-compressed) at `dpi`.
fn encode_tiff_rgb(width: u32, height: u32, rgb: &[u8], dpi: f64) -> Result<Vec<u8>, String> {
    use tiff::encoder::{Compression, TiffEncoder, colortype::RGB8};
    let err = |e: tiff::TiffError| e.to_string();
    let mut out = std::io::Cursor::new(Vec::new());
    let mut tiff = TiffEncoder::new(&mut out).map_err(err)?.with_compression(Compression::Lzw);
    let mut image = tiff.new_image::<RGB8>(width, height).map_err(err)?;
    tiff_resolution(&mut image, dpi).map_err(err)?;
    image.write_data(rgb).map_err(err)?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod image_name_tests {
    use super::image_file_name;

    fn image(page: usize) -> asdreader_create::ExtractedImage {
        asdreader_create::ExtractedImage {
            page,
            object: asdreader_cos::ObjRef { num: 1, generation: 0 },
            width: 1,
            height: 1,
            extension: "png",
            data: Vec::new(),
        }
    }

    #[test]
    fn page_number_is_padded_to_the_page_count() {
        assert_eq!(image_file_name("doc", &image(0), 1, 1), "doc_Page_1_Image_0001.png");
        assert_eq!(image_file_name("doc", &image(0), 1, 9), "doc_Page_1_Image_0001.png");
        assert_eq!(image_file_name("doc", &image(6), 2, 20), "doc_Page_07_Image_0002.png");
        assert_eq!(image_file_name("doc", &image(6), 3, 120), "doc_Page_007_Image_0003.png");
        assert_eq!(image_file_name("doc", &image(119), 4, 120), "doc_Page_120_Image_0004.png");
        assert_eq!(image_file_name("doc", &image(0), 1, 0), "doc_Page_1_Image_0001.png");
    }
}

#[cfg(test)]
mod raster_tests {
    use super::{ImageFormat, encode_image, encode_png};

    const FORMATS: [ImageFormat; 3] = [ImageFormat::Png, ImageFormat::Jpeg { quality: 90 }, ImageFormat::Tiff];

    /// The colour type and pixels of a PNG.
    fn decode(png: &[u8]) -> (png::ColorType, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(frame.buffer_size());
        (frame.color_type, pixels)
    }

    /// A rendered page has no translucent pixel: it is written as RGB, each pixel exactly as
    /// rendered (the alpha channel would only make the file bigger and slower to write).
    #[test]
    fn opaque_rasters_are_written_as_rgb_with_their_exact_pixels() {
        let rgba = [[255, 255, 255, 255], [0, 0, 0, 255], [200, 30, 90, 255], [1, 2, 3, 255], [17, 250, 128, 255], [90, 90, 90, 255]];
        let (color, pixels) = decode(&encode_png(3, 2, rgba.as_flattened()).unwrap());
        assert_eq!(color, png::ColorType::Rgb);
        assert_eq!(pixels, rgba.iter().flat_map(|p| [p[0], p[1], p[2]]).collect::<Vec<_>>());
    }

    /// A raster with translucent pixels keeps its alpha, with the colours un-premultiplied.
    #[test]
    fn translucent_rasters_keep_straight_alpha() {
        let rgba = [[64, 0, 32, 128], [0, 0, 0, 0], [10, 20, 30, 255]];
        let (color, pixels) = decode(&encode_png(3, 1, rgba.as_flattened()).unwrap());
        assert_eq!(color, png::ColorType::Rgba);
        assert_eq!(pixels, [128, 0, 64, 128, 0, 0, 0, 0, 10, 20, 30, 255]);
    }

    /// A buffer that is not exactly `width × height` RGBA pixels is an error in every format,
    /// opaque or not. A partial trailing pixel used to be dropped and the rest written as a
    /// valid image, and a buffer too short for its size panicked in the JPEG and TIFF encoders.
    #[test]
    fn rasters_of_the_wrong_length_are_rejected() {
        let cases: [(&str, u32, u32, &[u8]); 7] = [
            ("opaque, a partial pixel after", 1, 1, &[10, 20, 30, 255, 0]),
            ("opaque, three bytes after", 1, 1, &[10, 20, 30, 255, 0, 0, 0]),
            ("translucent, a partial pixel after", 1, 1, &[10, 20, 30, 128, 0]),
            ("opaque, a pixel short", 2, 1, &[10, 20, 30, 255]),
            ("translucent, a pixel too many", 1, 1, &[10, 20, 30, 128, 1, 2, 3, 4]),
            ("empty", 1, 1, &[]),
            ("a size whose byte count overflows", u32::MAX, u32::MAX, &[10, 20, 30, 255]),
        ];
        for format in FORMATS {
            for (what, width, height, data) in cases {
                assert!(encode_image(width, height, data, format, 72.0).is_err(), "{}, {what}", format.label());
            }
        }
    }

    /// A raster of no pixels is the right length (no bytes), and refused before any encoder sees
    /// it, in every format.
    #[test]
    fn rasters_of_no_pixels_are_an_error() {
        for format in FORMATS {
            for (width, height) in [(0, 0), (0, 3), (3, 0)] {
                let refused = encode_image(width, height, &[], format, 72.0).err();
                assert_eq!(refused, Some(format!("a {width}×{height} raster has no pixels")), "{}", format.label());
            }
        }
    }
}
