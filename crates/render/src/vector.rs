//! The vector content of a page, for exporting it to CAD (DXF): what the page draws, as paths,
//! text runs and images in displayed-page points (y down, `/Rotate` and `/UserUnit` applied),
//! with the colour, line width, dash pattern and layer (optional content group) each was drawn
//! with.
//!
//! The page is interpreted as for rendering, so forms, Type 3 glyphs, hidden layers and the
//! clip all apply. A clip is applied by its bounding box (exactly for the rectangles CAD
//! programs clip sheets and viewports with): what lies outside it is cut away. Text is kept as
//! text, one run per line of glyphs set in one font, size and colour, unless it can't be (no
//! Unicode, vertical or mirrored writing) or the caller asks for outlines; then its glyph
//! outlines are filled paths. Tiling patterns and shadings are not converted: they are counted
//! in [`Skipped`].

use std::collections::HashMap;
use std::sync::Arc;

use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::hayro_syntax::{Filter, Pdf};
use hayro::hayro_interpret::{
    BlendMode, CacheKey, ClipPath, Context, Device, FillRule, GlyphDrawMode, Image, ImageData, InterpreterCache, Paint, PathDrawMode, SoftMask,
    interpret_page,
};
use kurbo::{Affine, BezPath, PathEl, Point, Rect, Shape};

use crate::RenderConfig;

/// A point in displayed-page points, y down.
pub type Pt = [f64; 2];

/// Most items one page yields; past it the rest of the page is left out ([`VectorPage::truncated`]).
const MAX_ITEMS: usize = 2_000_000;
/// Most path points one page yields.
const MAX_POINTS: usize = 40_000_000;
/// Most image pixels one page yields (all its images together).
const MAX_IMAGE_PIXELS: u64 = 1 << 28;
/// How closely curves are followed where they are cut by a clip, in points.
const CLIP_TOLERANCE: f64 = 0.02;

/// One piece of a subpath, from the end of the previous one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Line(Pt),
    Cubic(Pt, Pt, Pt),
}

impl Seg {
    pub fn end(&self) -> Pt {
        match *self {
            Seg::Line(p) | Seg::Cubic(_, _, p) => p,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Subpath {
    pub start: Pt,
    pub segs: Vec<Seg>,
    /// Closed with `h` (or by a fill): the last point joins the start.
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    /// Line width in points on the page; 0 is the thinnest line the device can draw.
    pub width: f64,
    /// Dash lengths in points on the page (on, off, on, …); empty for a solid line.
    pub dash: Vec<f64>,
    pub dash_offset: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PathPaint {
    Stroke(Stroke),
    Fill { even_odd: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathItem {
    pub subpaths: Vec<Subpath>,
    pub paint: PathPaint,
    /// sRGB.
    pub color: [u8; 3],
    /// Index into [`VectorPage::layers`].
    pub layer: Option<usize>,
}

/// A run of text on one baseline, in one font, size and colour.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    pub text: String,
    /// Where the first glyph's baseline starts.
    pub origin: Pt,
    /// The em size in points.
    pub size: f64,
    /// The baseline direction (a unit vector, y down).
    pub direction: Pt,
    /// Glyph width as a fraction of its height: 1 unless the text is stretched (`Tz`, or a
    /// matrix that scales x and y differently).
    pub width_factor: f64,
    /// The slant of the glyphs from upright, in degrees (positive leans forward).
    pub oblique: f64,
    /// From the origin to where the last glyph ends, along the baseline, in points.
    pub length: f64,
    pub color: [u8; 3],
    /// Index into [`VectorPage::fonts`].
    pub font: usize,
    pub layer: Option<usize>,
}

/// The font a text run is set in.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFace {
    /// The family name the font gives itself (`TH SarabunPSK`, `Arial`), without a subset prefix.
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// The font's ascent (Windows `usWinAscent`, else `hhea` ascender) per em: CAD programs size
    /// TrueType text by it.
    pub ascent: f64,
}

/// An image a page draws, once however often it is placed.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorImage {
    pub width: u32,
    pub height: u32,
    /// Straight (not premultiplied) RGBA, row by row from the top.
    pub rgba: Vec<u8>,
    /// The image's own JPEG data, when it can be written unchanged (DCT-coded gray or RGB with
    /// no mask or decode array).
    pub jpeg: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageItem {
    /// Index into [`VectorPage::images`].
    pub image: usize,
    /// Pixel space (x right, y down from the top-left corner, one unit per pixel) to the page,
    /// as `[a, b, c, d, e, f]`.
    pub transform: [f64; 6],
    /// The part of the page the image is clipped to, `[x0, y0, x1, y1]`, when the clip cuts it.
    pub clip: Option<[f64; 4]>,
    pub layer: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VectorItem {
    Path(PathItem),
    Text(TextItem),
    Image(ImageItem),
}

/// Content a page draws that isn't in [`VectorPage::items`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Skipped {
    /// Fills and strokes painted with a tiling pattern or a shading.
    pub patterns: usize,
    /// Images over the page's pixel budget, or that couldn't be decoded.
    pub images: usize,
    /// The page has more than [`MAX_ITEMS`] items or [`MAX_POINTS`] points; the rest is left out.
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VectorPage {
    /// The displayed page size in points.
    pub width: f64,
    pub height: f64,
    /// In drawing order.
    pub items: Vec<VectorItem>,
    /// Layer names, in the order the page first uses them.
    pub layers: Vec<String>,
    pub fonts: Vec<FontFace>,
    pub images: Vec<VectorImage>,
    /// The text of runs drawn as outlines ([`VectorOptions::text_as_outlines`]), so what the
    /// page says (a stated scale) can still be read.
    pub outlined_text: Vec<String>,
    pub skipped: Skipped,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VectorOptions {
    /// All text as filled glyph outlines (looks exactly as on the page, can't be edited as text).
    pub text_as_outlines: bool,
}

/// The vector content of page `page` (0-based) of a PDF.
pub fn page_vectors(bytes: Arc<Vec<u8>>, config: &RenderConfig, page: usize, options: VectorOptions) -> Result<VectorPage, String> {
    let password = config.password.as_deref().unwrap_or("");
    let pdf = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Pdf::new_with_password(bytes, password).ok()))
        .ok()
        .flatten()
        .ok_or("the document could not be read")?;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract(&pdf, page, config, options)))
        .map_err(|p| format!("page {}: {}", page + 1, crate::raster::panic_message(&p)))?
}

fn extract(pdf: &Pdf, page: usize, config: &RenderConfig, options: VectorOptions) -> Result<VectorPage, String> {
    let pages = pdf.pages();
    let p = pages.get(page).ok_or_else(|| format!("page {} does not exist", page + 1))?;
    let (w, h) = p.render_dimensions();
    let cache = InterpreterCache::new();
    let initial = p.initial_transform(true).to_kurbo();
    let mut ctx = Context::new(initial, Rect::new(0.0, 0.0, f64::from(w), f64::from(h)), &cache, p.xref(), config.settings());
    let mut dev = VectorDevice::new(options);
    interpret_page(p, &mut ctx, &mut dev);
    dev.flush_text();
    Ok(VectorPage {
        width: f64::from(w),
        height: f64::from(h),
        items: dev.items,
        layers: dev.layers,
        fonts: dev.fonts,
        images: dev.images,
        outlined_text: dev.outlined_text,
        skipped: dev.skipped,
    })
}

trait ToKurbo {
    fn to_kurbo(self) -> Affine;
}

impl ToKurbo for hayro::hayro_interpret::hayro_syntax::transform::Transform {
    fn to_kurbo(self) -> Affine {
        Affine::new(self.as_coeffs())
    }
}

/// A text run being gathered glyph by glyph.
struct PendingRun {
    item: TextItem,
    /// Where the next glyph would start if it follows on.
    pen: Pt,
}

struct VectorDevice {
    options: VectorOptions,
    items: Vec<VectorItem>,
    points: usize,
    layers: Vec<String>,
    layer_ids: HashMap<String, usize>,
    /// Open layers: (the marked-content depth that ends it, or `None` for an XObject's, layer).
    layer_stack: Vec<(Option<usize>, usize)>,
    depth: usize,
    /// The bounding box of each clip pushed (y down), innermost last.
    clips: Vec<Rect>,
    fonts: Vec<FontFace>,
    font_ids: HashMap<u128, usize>,
    images: Vec<VectorImage>,
    image_ids: HashMap<u128, usize>,
    image_pixels: u64,
    run: Option<PendingRun>,
    outlined_text: Vec<String>,
    skipped: Skipped,
}

fn pt(p: Point) -> Pt {
    [p.x, p.y]
}

fn finite(p: Pt) -> bool {
    p[0].is_finite() && p[1].is_finite()
}

fn sub(a: Pt, b: Pt) -> Pt {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: Pt, b: Pt) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

/// How much a transform scales lengths (the square root of its area scale).
fn length_scale(t: Affine) -> f64 {
    let [a, b, c, d, _, _] = t.as_coeffs();
    (a * d - b * c).abs().sqrt()
}

/// `path` under `t`, as subpaths in page points. Quadratic curves become cubics.
fn subpaths(path: &BezPath, t: Affine) -> Vec<Subpath> {
    let mut out: Vec<Subpath> = Vec::new();
    let mut current: Option<Subpath> = None;
    let mut last = [0.0, 0.0];
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                if let Some(s) = current.take() {
                    out.push(s);
                }
                last = pt(t * p);
                current = Some(Subpath { start: last, segs: Vec::new(), closed: false });
            }
            PathEl::LineTo(p) => {
                let s = current.get_or_insert_with(|| Subpath { start: last, segs: Vec::new(), closed: false });
                last = pt(t * p);
                s.segs.push(Seg::Line(last));
            }
            PathEl::QuadTo(c, p) => {
                let s = current.get_or_insert_with(|| Subpath { start: last, segs: Vec::new(), closed: false });
                let (q0, q1, q2) = (last, pt(t * c), pt(t * p));
                let c1 = [q0[0] + 2.0 / 3.0 * (q1[0] - q0[0]), q0[1] + 2.0 / 3.0 * (q1[1] - q0[1])];
                let c2 = [q2[0] + 2.0 / 3.0 * (q1[0] - q2[0]), q2[1] + 2.0 / 3.0 * (q1[1] - q2[1])];
                last = q2;
                s.segs.push(Seg::Cubic(c1, c2, q2));
            }
            PathEl::CurveTo(a, b, p) => {
                let s = current.get_or_insert_with(|| Subpath { start: last, segs: Vec::new(), closed: false });
                last = pt(t * p);
                s.segs.push(Seg::Cubic(pt(t * a), pt(t * b), last));
            }
            PathEl::ClosePath => {
                if let Some(mut s) = current.take() {
                    s.closed = true;
                    last = s.start;
                    out.push(s);
                }
            }
        }
    }
    if let Some(s) = current {
        out.push(s);
    }
    out.retain(|s| finite(s.start) && s.segs.iter().all(|g| finite(g.end())));
    out
}

fn bounds(subpaths: &[Subpath]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    let mut add = |p: Pt| {
        let q = Point::new(p[0], p[1]);
        r = Some(r.map_or(Rect::from_points(q, q), |r| r.union_pt(q)));
    };
    for s in subpaths {
        add(s.start);
        for g in &s.segs {
            match *g {
                Seg::Line(p) => add(p),
                Seg::Cubic(a, b, p) => {
                    add(a);
                    add(b);
                    add(p);
                }
            }
        }
    }
    r
}

/// A subpath as points, its curves followed within `CLIP_TOLERANCE`.
fn flatten(s: &Subpath) -> Vec<Pt> {
    let mut path = BezPath::new();
    path.move_to(Point::new(s.start[0], s.start[1]));
    for g in &s.segs {
        match *g {
            Seg::Line(p) => path.line_to(Point::new(p[0], p[1])),
            Seg::Cubic(a, b, p) => path.curve_to(Point::new(a[0], a[1]), Point::new(b[0], b[1]), Point::new(p[0], p[1])),
        }
    }
    // Followed to a millionth of its size at most: a curve as long as a country is not cut
    // into billions of pieces.
    let extent = path.bounding_box();
    let tolerance = CLIP_TOLERANCE.max(extent.width().max(extent.height()) * 1e-6);
    let tolerance = if tolerance.is_finite() { tolerance } else { return std::iter::once(s.start).chain(s.segs.iter().map(Seg::end)).collect() };
    let mut points = Vec::new();
    kurbo::flatten(path, tolerance, |el| match el {
        PathEl::MoveTo(p) | PathEl::LineTo(p) => points.push(pt(p)),
        _ => {}
    });
    points
}

/// The part of the line `a`–`b` inside `r` (Liang–Barsky), as parameters along it.
fn clip_line(a: Pt, b: Pt, r: Rect) -> Option<(f64, f64)> {
    let d = sub(b, a);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-d[0], a[0] - r.x0), (d[0], r.x1 - a[0]), (-d[1], a[1] - r.y0), (d[1], r.y1 - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

fn lerp(a: Pt, b: Pt, t: f64) -> Pt {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Stroked subpaths cut to `r`: the pieces inside, as open polylines (a closed subpath that
/// crosses the edge opens where it does).
fn clip_strokes(subpaths: Vec<Subpath>, r: Rect) -> Vec<Subpath> {
    let mut out = Vec::new();
    for s in subpaths {
        let mut points = flatten(&s);
        if s.closed {
            points.push(s.start);
        }
        let mut piece: Vec<Pt> = Vec::new();
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            match clip_line(a, b, r) {
                Some((t0, t1)) => {
                    let (p, q) = (lerp(a, b, t0), lerp(a, b, t1));
                    if piece.last() != Some(&p) {
                        if piece.len() > 1 {
                            out.push(polyline(std::mem::take(&mut piece)));
                        }
                        piece = vec![p];
                    }
                    piece.push(q);
                    if t1 < 1.0 {
                        out.push(polyline(std::mem::take(&mut piece)));
                    }
                }
                None => {
                    if piece.len() > 1 {
                        out.push(polyline(std::mem::take(&mut piece)));
                    }
                    piece.clear();
                }
            }
        }
        if piece.len() > 1 {
            out.push(polyline(piece));
        }
    }
    out.retain(|s| !s.segs.is_empty());
    out
}

fn polyline(points: Vec<Pt>) -> Subpath {
    let mut it = points.into_iter();
    let start = it.next().unwrap_or([0.0, 0.0]);
    Subpath { start, segs: it.map(Seg::Line).collect(), closed: false }
}

/// Filled subpaths cut to `r` (Sutherland–Hodgman, one loop at a time: the fill's parity inside
/// `r` is unchanged).
fn clip_fills(subpaths: Vec<Subpath>, r: Rect) -> Vec<Subpath> {
    let mut out = Vec::new();
    for s in subpaths {
        let mut poly = flatten(&s);
        type Inside = fn(Pt, Rect) -> bool;
        type Cross = fn(Pt, Pt, Rect) -> Pt;
        let edges: [(Inside, Cross); 4] = [
            (|p, r| p[0] >= r.x0, |a, b, r| lerp(a, b, (r.x0 - a[0]) / (b[0] - a[0]))),
            (|p, r| p[0] <= r.x1, |a, b, r| lerp(a, b, (r.x1 - a[0]) / (b[0] - a[0]))),
            (|p, r| p[1] >= r.y0, |a, b, r| lerp(a, b, (r.y0 - a[1]) / (b[1] - a[1]))),
            (|p, r| p[1] <= r.y1, |a, b, r| lerp(a, b, (r.y1 - a[1]) / (b[1] - a[1]))),
        ];
        for (inside, cross) in edges {
            let Some(&last) = poly.last() else { break };
            let mut next = Vec::with_capacity(poly.len() + 4);
            let mut prev = last;
            for &p in &poly {
                match (inside(prev, r), inside(p, r)) {
                    (true, true) => next.push(p),
                    (true, false) => next.push(cross(prev, p, r)),
                    (false, true) => {
                        next.push(cross(prev, p, r));
                        next.push(p);
                    }
                    (false, false) => {}
                }
                prev = p;
            }
            poly = next;
        }
        if poly.len() >= 3 {
            let mut s = polyline(poly);
            s.closed = true;
            out.push(s);
        }
    }
    out
}

/// The family name, weight, slant and ascent of an embedded TrueType or OpenType font.
fn face_of(g: &hayro::hayro_interpret::font::OutlineGlyph) -> FontFace {
    use skrifa::MetadataProvider;
    use skrifa::raw::TableProvider;
    let data = g.font_data();
    let ps_name = data.as_ref().and_then(|d| d.postscript_name.clone()).unwrap_or_default();
    let ps_family = {
        let name = match ps_name.split_once('+') {
            Some((prefix, rest)) if prefix.len() == 6 => rest,
            _ => ps_name.as_str(),
        };
        name.split(['-', ',']).next().unwrap_or(name).to_string()
    };
    let mut face = FontFace {
        family: ps_family,
        bold: data.as_ref().and_then(|d| d.weight).is_some_and(|w| w >= 600),
        italic: data.as_ref().is_some_and(|d| d.is_italic),
        ascent: 0.9,
    };
    let Some(data) = data else {
        if face.family.is_empty() {
            face.family = "Arial".into();
        }
        return face;
    };
    let bytes: &[u8] = (*data.data).as_ref();
    if let Ok(font) = skrifa::FontRef::from_index(bytes, 0) {
        let english_or_first = |id| font.localized_strings(id).english_or_first().map(|s| s.chars().collect::<String>());
        if let Some(family) = english_or_first(skrifa::string::StringId::FAMILY_NAME).filter(|f| !f.trim().is_empty()) {
            face.family = family.trim().to_string();
        }
        let upem = font.head().map(|h| f64::from(h.units_per_em())).unwrap_or(1000.0).max(1.0);
        let win = font.os2().map(|o| f64::from(o.us_win_ascent())).unwrap_or(0.0);
        let hhea = font.hhea().map(|h| f64::from(h.ascender().to_i16())).unwrap_or(0.0);
        let ascent = if win > 0.0 { win } else { hhea };
        if ascent > 0.0 && (ascent / upem).is_finite() {
            face.ascent = (ascent / upem).clamp(0.3, 3.0);
        }
        if let Ok(os2) = font.os2() {
            face.bold = os2.us_weight_class() >= 600 || face.bold;
            face.italic = os2.fs_selection().contains(skrifa::raw::tables::os2::SelectionFlags::ITALIC) || face.italic;
        }
    }
    if face.family.is_empty() {
        face.family = "Arial".into();
    }
    face
}

/// The leading bytes of a JPEG say how many colour components it has (its first frame header).
fn jpeg_components(data: &[u8]) -> Option<u8> {
    let mut i = 2usize;
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    while i + 4 <= data.len() {
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        let len = usize::from(u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]));
        // SOF0..SOF15 other than DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return data.get(i + 9).copied();
        }
        i = i.checked_add(2)?.checked_add(len)?;
    }
    None
}

impl VectorDevice {
    fn new(options: VectorOptions) -> Self {
        Self {
            options,
            items: Vec::new(),
            points: 0,
            layers: Vec::new(),
            layer_ids: HashMap::new(),
            layer_stack: Vec::new(),
            depth: 0,
            clips: Vec::new(),
            fonts: Vec::new(),
            font_ids: HashMap::new(),
            images: Vec::new(),
            image_ids: HashMap::new(),
            image_pixels: 0,
            run: None,
            outlined_text: Vec::new(),
            skipped: Skipped::default(),
        }
    }

    fn layer(&self) -> Option<usize> {
        self.layer_stack.last().map(|(_, l)| *l)
    }

    fn layer_id(&mut self, name: &[u8]) -> usize {
        let name = asdreader_cos::PdfString::literal(name).to_text();
        let name = if name.trim().is_empty() { "Layer".to_string() } else { name };
        if let Some(&id) = self.layer_ids.get(&name) {
            return id;
        }
        let id = self.layers.len();
        self.layers.push(name.clone());
        self.layer_ids.insert(name, id);
        id
    }

    /// The clip's box, when one is set.
    fn clip(&self) -> Option<Rect> {
        self.clips.last().copied()
    }

    fn room(&mut self, points: usize) -> bool {
        if self.items.len() >= MAX_ITEMS || self.points.saturating_add(points) > MAX_POINTS {
            self.skipped.truncated = true;
            return false;
        }
        self.points += points;
        true
    }

    fn push(&mut self, item: VectorItem) {
        // Text drawn as outlines is gathered only to be read: its own outlines don't end its run.
        if !self.options.text_as_outlines {
            self.flush_text();
        }
        let points = match &item {
            VectorItem::Path(p) => p.subpaths.iter().map(|s| s.segs.len() * 3 + 1).sum(),
            _ => 1,
        };
        if self.room(points) {
            self.items.push(item);
        }
    }

    fn flush_text(&mut self) {
        let Some(run) = self.run.take() else { return };
        if run.item.text.trim().is_empty() {
            return;
        }
        if self.options.text_as_outlines {
            if self.outlined_text.len() < MAX_ITEMS {
                self.outlined_text.push(run.item.text);
            }
        } else if self.room(1) {
            self.items.push(VectorItem::Text(run.item));
        }
    }

    fn add_path(&mut self, path: &BezPath, transform: Affine, color: [u8; 3], paint: PathPaint) {
        let mut subpaths = subpaths(path, transform);
        if let PathPaint::Fill { .. } = paint {
            for s in &mut subpaths {
                s.closed = true;
            }
        }
        let Some(b) = bounds(&subpaths) else { return };
        if let Some(clip) = self.clip() {
            let inside = b.x0 >= clip.x0 - 1e-6 && b.y0 >= clip.y0 - 1e-6 && b.x1 <= clip.x1 + 1e-6 && b.y1 <= clip.y1 + 1e-6;
            if !inside {
                if b.x1 < clip.x0 || b.x0 > clip.x1 || b.y1 < clip.y0 || b.y0 > clip.y1 {
                    return;
                }
                subpaths = match paint {
                    PathPaint::Stroke(_) => clip_strokes(subpaths, clip),
                    PathPaint::Fill { .. } => clip_fills(subpaths, clip),
                };
            }
        }
        if subpaths.is_empty() {
            return;
        }
        let layer = self.layer();
        self.push(VectorItem::Path(PathItem { subpaths, paint, color, layer }));
    }

    fn font_id(&mut self, g: &hayro::hayro_interpret::font::OutlineGlyph) -> usize {
        let key = g.font_cache_key();
        if let Some(&id) = self.font_ids.get(&key) {
            return id;
        }
        let id = self.fonts.len();
        self.fonts.push(face_of(g));
        self.font_ids.insert(key, id);
        id
    }

    fn add_image(&mut self, key: u128, decode: impl FnOnce() -> Option<VectorImage>) -> Option<usize> {
        if let Some(&id) = self.image_ids.get(&key) {
            return Some(id);
        }
        let image = decode()?;
        let pixels = u64::from(image.width) * u64::from(image.height);
        if self.image_pixels.saturating_add(pixels) > MAX_IMAGE_PIXELS {
            self.skipped.images += 1;
            return None;
        }
        self.image_pixels += pixels;
        let id = self.images.len();
        self.images.push(image);
        self.image_ids.insert(key, id);
        Some(id)
    }

    /// A glyph drawn as text: joins the run being gathered when it continues that run's line.
    fn add_glyph_text(&mut self, text: String, t: Affine, advance: f64, font: usize, color: [u8; 3]) -> bool {
        let origin = pt(t * Point::ZERO);
        let ex = sub(pt(t * Point::new(1.0, 0.0)), origin);
        let ey = sub(pt(t * Point::new(0.0, 1.0)), origin);
        let (xs, ys) = (ex[0].hypot(ex[1]), ey[0].hypot(ey[1]));
        if !(xs.is_finite() && ys.is_finite() && xs > 0.0 && ys > 0.0 && finite(origin)) {
            return false;
        }
        let direction = [ex[0] / xs, ex[1] / xs];
        let up = [ey[0] / ys, ey[1] / ys];
        // Glyph space is y up; on the page (y down) upright text has `up` a quarter turn
        // counterclockwise from the baseline. The other way round, the text is mirrored.
        let normal = [direction[1], -direction[0]];
        let upright = dot(normal, up);
        if upright <= 0.05 {
            return false;
        }
        let oblique = (dot(direction, up)).atan2(upright).to_degrees();
        // Glyph space has 1000 units per em.
        let size = ys * upright * 1000.0;
        let width_factor = xs / (ys * upright);
        let advance = advance * xs;
        let layer = self.layer();
        if let Some(run) = &mut self.run {
            let r = &run.item;
            let same = r.font == font
                && r.color == color
                && r.layer == layer
                && (r.size - size).abs() <= r.size * 0.01
                && dot(r.direction, direction) > 0.9999
                && (r.width_factor - width_factor).abs() <= 0.01
                && (r.oblique - oblique).abs() <= 0.5;
            let delta = sub(origin, run.pen);
            let along = dot(delta, r.direction);
            let across = dot(delta, [r.direction[1], -r.direction[0]]);
            if same && across.abs() <= 0.05 * size && along >= -0.15 * size && along <= 0.8 * size {
                // A gap of more than a narrow space between words is a space in the text.
                if along > 0.15 * size && !run.item.text.ends_with(' ') && !text.starts_with(' ') {
                    run.item.text.push(' ');
                }
                run.item.text.push_str(&text);
                let end = [origin[0] + direction[0] * advance, origin[1] + direction[1] * advance];
                if advance > 0.0 {
                    run.pen = end;
                }
                run.item.length = run.item.length.max(dot(sub(end, run.item.origin), run.item.direction));
                return true;
            }
        }
        self.flush_text();
        let end = [origin[0] + direction[0] * advance, origin[1] + direction[1] * advance];
        self.run = Some(PendingRun {
            item: TextItem { text, origin, size, direction, width_factor, oblique, length: advance.max(0.0), color, font, layer },
            pen: end,
        });
        true
    }
}

fn rgb(paint: &Paint<'_>) -> Option<[u8; 3]> {
    match paint {
        Paint::Color(c) => {
            let [r, g, b, a] = c.to_rgba().to_rgba8();
            (a > 0).then_some([r, g, b])
        }
        Paint::Pattern(_) => None,
    }
}

fn is_thai_lao_mark(c: char) -> bool {
    matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E3A}' | '\u{0E47}'..='\u{0E4E}' | '\u{0EB1}' | '\u{0EB4}'..='\u{0EBC}' | '\u{0EC8}'..='\u{0ECE}')
}

impl<'a> Device<'a> for VectorDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        let Some(color) = rgb(paint) else {
            if matches!(paint, Paint::Pattern(_)) {
                self.skipped.patterns += 1;
            }
            return;
        };
        let paint = match draw_mode {
            PathDrawMode::Fill(rule) => PathPaint::Fill { even_odd: matches!(rule, FillRule::EvenOdd) },
            PathDrawMode::Stroke(s) => {
                let scale = length_scale(transform);
                let width = f64::from(s.line_width) * scale;
                let dash: Vec<f64> = s.dash_array.iter().map(|d| f64::from(*d).abs() * scale).collect();
                // A dash array of zeros (or one that can't be drawn) is a solid line.
                let dash = if dash.iter().all(|d| d.is_finite()) && dash.iter().sum::<f64>() > 0.0 { dash } else { Vec::new() };
                PathPaint::Stroke(Stroke {
                    width: if width.is_finite() { width.max(0.0) } else { 0.0 },
                    dash,
                    dash_offset: (f64::from(s.dash_offset) * scale).max(0.0),
                })
            }
        };
        self.add_path(path, transform, color, paint);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        let b = clip_path.path.bounding_box();
        let b = match self.clip() {
            Some(outer) => {
                let i = outer.intersect(b);
                if i.width() < 0.0 || i.height() < 0.0 { Rect::new(i.x0, i.y0, i.x0, i.y0) } else { i }
            }
            None => b,
        };
        self.clips.push(b);
    }

    fn pop_clip_path(&mut self) {
        self.clips.pop();
    }

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(&mut self, glyph: &Glyph<'a>, transform: Affine, glyph_transform: Affine, paint: &Paint<'a>, draw_mode: &GlyphDrawMode) {
        if matches!(draw_mode, GlyphDrawMode::Invisible | GlyphDrawMode::Clip) {
            return;
        }
        let g = match glyph {
            Glyph::Type3(g) => {
                // Drawn with its own paths and images.
                self.flush_text();
                g.interpret(self, transform, glyph_transform, paint);
                return;
            }
            Glyph::Outline(g) => g,
        };
        let Some(color) = rgb(paint) else {
            if matches!(paint, Paint::Pattern(_)) {
                self.skipped.patterns += 1;
            }
            return;
        };
        let t = transform * glyph_transform;
        let text = match glyph.as_unicode() {
            Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => Some(c.to_string()),
            Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => Some(s),
            None => None,
        }
        .map(|text| {
            let text = asdreader_fonts::thai_pua::normalize(&text).into_owned();
            text.chars().map(|c| if c == '\u{a0}' || c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect::<String>()
        });
        let origin = pt(t * Point::ZERO);
        let clipped_out = self.clip().is_some_and(|c| !c.inflate(0.5, 0.5).contains(Point::new(origin[0], origin[1])));
        if clipped_out {
            return;
        }
        if let Some(text) = text.filter(|t| !t.is_empty())
            && !g.is_vertical()
        {
            let combining = text.chars().all(is_thai_lao_mark);
            let advance = match g.advance_width() {
                Some(a) if a > 0.0 && !combining => f64::from(a),
                Some(_) | None if combining => 0.0,
                _ => g.outline().bounding_box().width().max(0.0),
            };
            let font = self.font_id(g);
            if self.add_glyph_text(text, t, advance, font, color) && !self.options.text_as_outlines {
                return;
            }
        }
        // As its outline.
        let outline = g.outline();
        if outline.elements().is_empty() {
            return;
        }
        let paint = match draw_mode {
            GlyphDrawMode::Stroke(s) => {
                PathPaint::Stroke(Stroke { width: (f64::from(s.line_width) * length_scale(transform)).max(0.0), dash: Vec::new(), dash_offset: 0.0 })
            }
            _ => PathPaint::Fill { even_odd: false },
        };
        self.add_path(&outline, t, color, paint);
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        self.flush_text();
        let key = image.cache_key();
        let mut transform = transform;
        let id = match &image {
            Image::Stencil(s) => {
                let Some(color) = (match s_paint_color(s) {
                    Some(c) => Some(c),
                    None => {
                        self.skipped.patterns += 1;
                        None
                    }
                }) else {
                    return;
                };
                let key = key ^ (u128::from(color[0]) << 16 | u128::from(color[1]) << 8 | u128::from(color[2]));
                let mut scale = (1.0f32, 1.0f32);
                let id = self.add_image(key, || {
                    let mut out = None;
                    s.with_stencil(
                        |luma, _| {
                            scale = luma.scale_factors;
                            let rgba = luma.data.iter().flat_map(|a| [color[0], color[1], color[2], *a]).collect();
                            out = Some(VectorImage { width: luma.width, height: luma.height, rgba, jpeg: None });
                        },
                        None,
                    );
                    out
                });
                transform *= Affine::scale_non_uniform(f64::from(scale.0), f64::from(scale.1));
                id
            }
            Image::Raster(r) => {
                let mut scale = (1.0f32, 1.0f32);
                let jpeg = {
                    let stream = r.stream();
                    let filters = stream.filters();
                    (filters.as_slice() == [Filter::DctDecode]
                        && stream.dict().get::<hayro::hayro_interpret::hayro_syntax::object::Array<'_>>(b"Decode").is_none())
                    .then(|| stream.raw_data().into_owned())
                };
                let id = self.add_image(key, || {
                    let mut out = None;
                    r.with_rgba(
                        |data, alpha| {
                            scale = data.scale_factors();
                            let (width, height) = (data.width(), data.height());
                            let n = (width as usize).saturating_mul(height as usize);
                            let mut rgba = Vec::new();
                            if rgba.try_reserve_exact(n.saturating_mul(4)).is_err() {
                                return;
                            }
                            let components = match &data {
                                ImageData::Rgb(d) => {
                                    rgba.extend(d.data.as_chunks::<3>().0.iter().flat_map(|[r, g, b]| [*r, *g, *b, 255]));
                                    3
                                }
                                ImageData::Luma(d) => {
                                    rgba.extend(d.data.iter().flat_map(|v| [*v, *v, *v, 255]));
                                    1
                                }
                            };
                            rgba.resize(n * 4, 255);
                            let masked = alpha.is_some();
                            if let Some(a) = alpha
                                && a.width == width
                                && a.height == height
                            {
                                for (px, a) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(a.data.iter()) {
                                    px[3] = *a;
                                }
                            }
                            let jpeg = jpeg.filter(|j| !masked && scale == (1.0, 1.0) && jpeg_components(j) == Some(components));
                            out = Some(VectorImage { width, height, rgba, jpeg });
                        },
                        None,
                    );
                    out
                });
                transform *= Affine::scale_non_uniform(f64::from(scale.0), f64::from(scale.1));
                id
            }
        };
        let Some(image) = id else {
            return;
        };
        let Some(info) = self.images.get(image) else { return };
        let (w, h) = (f64::from(info.width), f64::from(info.height));
        // Each pixel's size is part of the transform the image was decoded at.
        let corners = [Point::new(0.0, 0.0), Point::new(w, 0.0), Point::new(w, h), Point::new(0.0, h)].map(|p| transform * p);
        let b = corners.iter().fold(Rect::from_points(corners[0], corners[0]), |r, p| r.union_pt(*p));
        let mut clip = None;
        if let Some(c) = self.clip() {
            if b.x1 < c.x0 || b.x0 > c.x1 || b.y1 < c.y0 || b.y0 > c.y1 {
                return;
            }
            if !(b.x0 >= c.x0 - 1e-6 && b.y0 >= c.y0 - 1e-6 && b.x1 <= c.x1 + 1e-6 && b.y1 <= c.y1 + 1e-6) {
                clip = Some([c.x0, c.y0, c.x1, c.y1]);
            }
        }
        let layer = self.layer();
        self.push(VectorItem::Image(ImageItem { image, transform: transform.as_coeffs(), clip, layer }));
    }

    fn begin_marked_content(&mut self, _: &[u8], _: Option<i32>) {
        self.depth = self.depth.saturating_add(1);
    }

    fn marked_content_layer(&mut self, name: &[u8]) {
        let id = self.layer_id(name);
        self.layer_stack.push((Some(self.depth), id));
    }

    fn end_marked_content(&mut self) {
        if self.layer_stack.last().is_some_and(|(d, _)| *d == Some(self.depth)) {
            self.layer_stack.pop();
        }
        self.depth = self.depth.saturating_sub(1);
    }

    fn begin_layer(&mut self, name: &[u8]) {
        let id = self.layer_id(name);
        self.layer_stack.push((None, id));
    }

    fn end_layer(&mut self) {
        if let Some(i) = self.layer_stack.iter().rposition(|(d, _)| d.is_none()) {
            self.layer_stack.truncate(i);
        }
    }
}

/// A stencil mask's paint colour: the fill colour it is painted with.
fn s_paint_color(s: &hayro::hayro_interpret::StencilImage<'_, '_>) -> Option<[u8; 3]> {
    let mut color = None;
    s.with_stencil(|_, paint| color = rgb(paint), Some((1, 1)));
    color
}

#[cfg(test)]
mod tests;
