use crate::{BlendMode, FillRule};
use crate::color::{ColorSpace, DefaultColorSpaces, DeviceFamily};
use crate::context::Context;
use crate::convert::{convert_line_cap, convert_line_join};
use crate::device::Device;
use crate::font::{Font, FontData, FontQuery, StandardFont};
use crate::interpret::path::{
    apply_pending_clip, close_path, fill_path, fill_path_impl, fill_stroke_path, stroke_path,
};
use crate::interpret::state::{TextStateFont, handle_gs};
use crate::interpret::text::TextRenderingMode;
use crate::pattern::{Pattern, ShadingPattern};
use crate::shading::Shading;
use crate::util::{OptionLog, RectExt};
use crate::x_object::{
    FormXObject, ImageXObject, XObject, draw_form_xobject, draw_image_xobject, draw_xobject,
};
use hayro_syntax::content::TypedIter;
use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::object::dict::keys::{
    ACTUAL_TEXT, ANNOTS, AP, AS, C, CA, F, MCID, N, OC, QUADPOINTS, RECT, SUBTYPE,
};
use hayro_syntax::object::{Array, Dict, Object, Rect, Stream, dict_or_stream};
use hayro_syntax::page::{ContentReader, Page, Resources};
use hayro_syntax::MAX_DECODED_STREAM;
use kurbo::{Affine, BezPath, Point, Shape};
use smallvec::smallvec;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) mod path;
pub(crate) mod state;
pub(crate) mod text;

pub use state::ActiveTransferFunction;

/// A callback function for resolving font queries.
///
/// The first argument is the raw data, the second argument is the index in case the font
/// is a TTC, otherwise it should be 0.
pub type FontResolverFn = Arc<dyn Fn(&FontQuery) -> Option<(FontData, u32)> + Send + Sync>;
/// A callback function for resolving cmap names to their files.
pub type CMapResolverFn =
    Arc<dyn Fn(hayro_cmap::CMapName<'_>) -> Option<&'static [u8]> + Send + Sync>;
/// A callback function for resolving warnings during interpretation.
pub type WarningSinkFn = Arc<dyn Fn(InterpreterWarning) + Send + Sync>;

#[derive(Clone)]
/// Settings that should be applied during the interpretation process.
pub struct InterpreterSettings {
    /// Nearly every PDF contains text. In most cases, PDF files embed the fonts they use, and
    /// hayro can therefore read the font files and do all the processing needed. However, there
    /// are two problems:
    /// - Fonts don't _have_ to be embedded, it's possible that the PDF file only defines the basic
    ///   metadata of the font, like its name, but relies on the PDF processor to find that font
    ///   in its environment.
    /// - The PDF specification requires a list of 14 fonts that should always be available to a
    ///   PDF processor. These include:
    ///   - Times New Roman (Normal, Bold, Italic, `BoldItalic`)
    ///   - Courier (Normal, Bold, Italic, `BoldItalic`)
    ///   - Helvetica (Normal, Bold, Italic, `BoldItalic`)
    ///   - `ZapfDingBats`
    ///   - Symbol
    ///
    /// Because of this, if any of the above situations occurs, this callback will be called, which
    /// expects the data of an appropriate font to be returned, if available. If no such font is
    /// provided, the text will most likely fail to render.
    ///
    /// For the font data, there are two different formats that are accepted:
    /// - Any valid TTF/OTF font.
    /// - A valid CFF font program.
    ///
    /// The following recommendations are given for the implementation of this callback function.
    ///
    /// For the standard fonts, in case the original fonts are available on the system, you should
    /// just return those. Otherwise, for Helvetica, Courier and Times New Roman, the best alternative
    /// are the corresponding fonts of the [Liberation font family](https://github.com/liberationfonts/liberation-fonts).
    /// If you prefer smaller fonts, you can use the [Foxit CFF fonts](https://github.com/LaurenzV/hayro/tree/master/assets/standard_fonts),
    /// which are much smaller but are missing glyphs for certain scripts.
    ///
    /// For the `Symbol` and `ZapfDingBats` fonts, you should also prefer the system fonts, and if
    /// not available to you, you can, similarly to above, use the corresponding fonts from Foxit.
    ///
    /// If you don't want having to deal with this, you can just enable the `embed-fonts` feature
    /// and use the default implementation of the callback.
    pub font_resolver: FontResolverFn,
    /// A callback for resolving cmaps that aren't embedded.
    ///
    /// When the PDF requires using a cmap that is not directly embedded in the PDF,
    /// this callback will be called to attempt fetching the data of the file.
    ///
    /// When the `embed-cmaps` feature is enabled, this uses `load_embedded`
    /// method from `hayro-cmap` by default, which embeds the cmap files for
    /// all 61 predefined cmaps
    /// that the PDF specification requires to be readily available on a system.
    /// Otherwise, you can implement your custom logic for lazily fetching the
    /// data. If you are fine not supporting such PDFs, you can simply pass a closure
    /// that always returns `None`.
    pub cmap_resolver: CMapResolverFn,
    /// In certain cases, `hayro` will emit a warning in case an issue was encountered while interpreting
    /// the PDF file. Providing a callback allows you to catch those warnings and handle them, if desired.
    pub warning_sink: WarningSinkFn,
    /// Whether annotations should be rendered as well.
    ///
    /// Note that this feature is currently not fully implemented yet, so some
    /// annotations might be missing.
    pub render_annotations: bool,
    /// ASDReader patch: hide comments (markup annotations) but keep form fields (`/Widget`)
    /// and links, for the viewer's "Hide all comments".
    pub hide_comments: bool,
    /// ASDReader patch: viewer overrides for optional content groups (object number, generation,
    /// visible), applied on top of the document's default configuration (Layers panel toggles).
    pub ocg_overrides: Arc<Vec<(i32, i32, bool)>>,
    /// ASDReader patch: once this is `true`, interpretation stops before the next content
    /// operator. What was drawn is then incomplete, so only set it when the output will be
    /// thrown away (a viewer's render whose result nobody can receive any more).
    pub cancelled: Option<Arc<AtomicBool>>,
    /// ASDReader patch: a low-detail render for previews (page backdrops, thumbnails). Images
    /// that would cover only a few device pixels are skipped: redrawing them at full detail
    /// costs a decode and a resample each for pixels nobody can see. Never set it for a final
    /// render (tiles, whole pages, print, export).
    pub preview: bool,
    /// ASDReader patch: make or use the index of the page's content that lets a render of part
    /// of the page skip what cannot reach it (see `content_index`). Only a page's own top-level
    /// content is indexed.
    pub content_index: crate::content_index::ContentIndexMode,
}

impl Default for InterpreterSettings {
    fn default() -> Self {
        Self {
            #[cfg(not(feature = "embed-fonts"))]
            font_resolver: Arc::new(|_| None),
            #[cfg(feature = "embed-fonts")]
            font_resolver: Arc::new(|query| match query {
                FontQuery::Standard(s) => Some(s.get_font_data()),
                // ASDReader patch: a simple font is asked for by name before its bundled stand-in
                // is chosen, and the stand-in is picked by the font itself when this resolver has
                // nothing better; so a simple font gets no answer here. A composite (Type 0) font
                // keeps the stand-in picked from its flags.
                FontQuery::Fallback(f) if !f.is_composite => None,
                FontQuery::Fallback(f) => Some(f.pick_standard_font().get_font_data()),
            }),
            #[cfg(feature = "embed-cmaps")]
            cmap_resolver: Arc::new(hayro_cmap::load_embedded),
            #[cfg(not(feature = "embed-cmaps"))]
            cmap_resolver: Arc::new(|_| None),
            warning_sink: Arc::new(|_| {}),
            render_annotations: true,
            hide_comments: false,
            ocg_overrides: Arc::new(Vec::new()),
            cancelled: None,
            preview: false,
            content_index: crate::content_index::ContentIndexMode::Off,
        }
    }
}

#[derive(Copy, Clone, Debug)]
/// Warnings that can occur while interpreting a PDF file.
pub enum InterpreterWarning {
    /// An unsupported font kind was encountered.
    ///
    /// Currently, only CID fonts with non-identity encoding are unsupported.
    UnsupportedFont,
    /// An image failed to decode.
    ImageDecodeFailure,
    /// ASDReader patch (18): a page content stream was skipped after exhausting its safety budget.
    ContentTruncated,
}

/// interpret the contents of the page and render them into the device.
pub fn interpret_page<'a>(
    page: &Page<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
) {
    let resources = page.resources();
    let mut content = page.content_reader();
    // ASDReader patch: the page's own contents count against its content budget too, where they
    // are decoded whole. Streamed content is only ever held a window at a time.
    crate::context::charge_content(content.materialized_len());
    // ASDReader patch: the page's content index (see `content_index`). Taken out of the settings
    // so that nothing nested (forms, patterns, masks, glyphs) sees it.
    let mode = core::mem::take(&mut context.settings.content_index);
    let initial = context.initial_transform();
    let to_page = initial.inverse();
    let page_box = to_page.transform_rect_bbox(context.bbox());
    let slot = match &mode {
        crate::content_index::ContentIndexMode::Record(slot) => Some(slot.clone()),
        _ => None,
    };
    let (mut indexing, collected) =
        crate::content_index::Indexing::new(mode, to_page, context.bbox(), page_box);
    match collected {
        Some(collected) => {
            let mut bounds = crate::content_index::BoundsDevice {
                inner: &mut *device,
                collecting: collected,
                to_page,
                glyph_bounds: std::collections::HashMap::new(),
            };
            interpret_content(&mut content, resources, context, &mut bounds, &mut indexing);
        }
        None => interpret_content(&mut content, resources, context, device, &mut indexing),
    }
    if let Some(slot) = slot {
        let cancelled = context.settings.cancelled.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
        let cut = content.truncated() || crate::context::content_was_truncated();
        if !cancelled && !cut && let Some(index) = indexing.finish() {
            *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(index));
        }
    }

    if context.settings.render_annotations
        && let Some(annot_arr) = page.raw().get::<Array<'_>>(ANNOTS)
    {
        // ASDReader patch (29): bracket the annotations so devices can tell their paints from
        // the document's own (the reading-colours setting recolours the document, not comments).
        device.begin_annotations();
        for annot in annot_arr.iter::<Dict<'_>>() {
            let flags = annot.get::<u32>(F).unwrap_or(0);

            // Annotation should be hidden (Hidden = bit 2, NoView = bit 6).
            // ASDReader patch: NoView was not honoured upstream.
            if flags & (2 | 32) != 0 {
                continue;
            }
            // ASDReader patch: "Hide all comments" keeps widgets and links.
            if context.settings.hide_comments
                && !matches!(annot.get::<hayro_syntax::object::Name<'_>>(SUBTYPE).as_deref(), Some(b"Widget" | b"Link"))
            {
                continue;
            }

            // ASDReader patch: `/N` may be a stream or a dictionary of appearance states selected
            // by `/AS` (checkboxes, radio buttons, …). Upstream only handled the stream form.
            let normal = annot.get::<Dict<'_>>(AP).and_then(|ap| {
                ap.get::<Stream<'_>>(N).or_else(|| {
                    let states = ap.get::<Dict<'_>>(N)?;
                    let state = annot.get::<hayro_syntax::object::Name<'_>>(AS)?;
                    states.get::<Stream<'_>>(state.as_ref())
                })
            });
            if let Some(apx) = normal.and_then(|o| FormXObject::new(&o)) {
                let Some(rect) = annot.get::<Rect>(RECT) else {
                    continue;
                };

                let annot_rect = rect.to_kurbo();
                // 12.5.5. Appearance streams
                // "The algorithm outlined in this subclause shall be used
                // to map from the coordinate system of the appearance XObject."

                // 1) The appearance’s bounding box (specified by its BBox entry)
                // shall be transformed, using Matrix, to produce a
                // quadrilateral with arbitrary orientation. The transformed
                // appearance box is the smallest upright rectangle that
                // encompasses this quadrilateral.
                let transformed_rect = (apx.matrix
                    * kurbo::Rect::new(
                        apx.bbox[0] as f64,
                        apx.bbox[1] as f64,
                        apx.bbox[2] as f64,
                        apx.bbox[3] as f64,
                    )
                    .to_path(0.1))
                .bounding_box();

                // 2) A matrix A shall be computed that scales and translates
                // the transformed appearance box to align with the edges
                // of the annotation’s rectangle (specified by the Rect entry).
                // A maps the lower-left corner (the corner with the smallest
                // x and y coordinates) and the upper-right corner (the
                // corner with the greatest x and y coordinates) of the
                // transformed appearance box to the corresponding corners
                // of the annotation’s rectangle.
                // ASDReader patch: the translation is taken after scaling (it was
                // `/Rect.x0 - box.x0`, right only for a box at the origin or a scale of 1).
                let (sx, sy) = (
                    annot_rect.width() / transformed_rect.width(),
                    annot_rect.height() / transformed_rect.height(),
                );
                let affine = Affine::new([
                    sx,
                    0.0,
                    0.0,
                    sy,
                    annot_rect.x0 - sx * transformed_rect.x0,
                    annot_rect.y0 - sy * transformed_rect.y0,
                ]);

                // ASDReader patch: a /BBox or /Matrix that collapses the appearance box to a line
                // or a point leaves nothing to map onto /Rect: the scales above divide by zero,
                // and the infinite or NaN matrix reached the device with everything the
                // appearance draws. Clipped to such a box the appearance shows nothing, so skip
                // it, as when the mapping overflows.
                if !(transformed_rect.is_finite()
                    && transformed_rect.width() > 0.0
                    && transformed_rect.height() > 0.0
                    && affine.is_finite())
                {
                    continue;
                }

                // 3) Matrix shall be concatenated with A to form a matrix
                // AA that maps from the appearance’s coordinate system to
                // the annotation’s rectangle in default user space.
                context.save_state();
                context.pre_concat_affine(affine);
                context.push_root_transform();

                draw_form_xobject(resources, &apx, context, device);
                context.pop_root_transform();
                context.restore_state(device);
            } else if annot.get::<hayro_syntax::object::Name<'_>>(SUBTYPE).as_deref() == Some(b"Highlight") {
                // ASDReader patch: a Highlight without a usable normal appearance was not drawn.
                draw_highlight_without_appearance(&annot, context, device);
            }
        }
        device.end_annotations();
    }
}

/// ASDReader patch: the normal appearance of a Highlight annotation that has none (ISO 32000-2
/// §12.5.6.10), drawn as ASDReader draws its own highlights (asdreader-annot): each /QuadPoints
/// quadrilateral filled with /C at /CA opacity, blended with Multiply so the marked-up content
/// stays legible. /QuadPoints that aren't groups of eight numbers, or a /C that isn't a gray, RGB
/// or CMYK colour (absent or empty means transparent), draw nothing.
fn draw_highlight_without_appearance<'a>(
    annot: &Dict<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
) {
    let numbers = |key| -> Option<Vec<f32>> {
        annot
            .get::<Array<'_>>(key)?
            .iter::<Object<'_>>()
            .map(|o| match o {
                Object::Number(n) => Some(n.as_f32()),
                _ => None,
            })
            .collect()
    };
    let Some(quad_points) = numbers(QUADPOINTS).filter(|q| !q.is_empty() && q.len() % 8 == 0)
    else {
        return;
    };
    let (color_space, color) = match numbers(C).as_deref() {
        Some(&[g]) => (ColorSpace::device_gray(), smallvec![g]),
        Some(&[r, g, b]) => (ColorSpace::device_rgb(), smallvec![r, g, b]),
        Some(&[c, m, y, k]) => (ColorSpace::device_cmyk(), smallvec![c, m, y, k]),
        _ => return,
    };

    let mut path = BezPath::new();
    // Corners in /QuadPoints order: upper left, upper right, lower left, lower right.
    for &[x1, y1, x2, y2, x3, y3, x4, y4] in quad_points.as_chunks::<8>().0 {
        path.move_to((x1 as f64, y1 as f64));
        path.line_to((x2 as f64, y2 as f64));
        path.line_to((x4 as f64, y4 as f64));
        path.line_to((x3 as f64, y3 as f64));
        path.close_path();
    }

    context.save_state();
    let state = &mut context.get_mut().graphics_state;
    state.none_stroke_cs = color_space;
    state.non_stroke_color = color;
    state.non_stroke_pattern = None;
    state.non_stroke_alpha = annot.get::<f32>(CA).unwrap_or(1.0).clamp(0.0, 1.0);
    state.soft_mask = None;
    state.blend_mode = BlendMode::Multiply;
    *context.path_mut() = path;
    fill_path(context, device, FillRule::NonZero);
    context.restore_state(device);
}

/// ASDReader patch (38): the colour space `cs` or `CS` selects by `name`: a device space, else
/// the `/ColorSpace` resource `name`. A device space, named directly or by a resource, is
/// replaced by its default from `defaults` (ISO 32000-2 §8.6.5.6).
fn named_color_space<'a>(
    name: &hayro_syntax::object::Name<'_>,
    resources: &Resources<'a>,
    defaults: &DefaultColorSpaces<'_, '_>,
    context: &mut Context<'a>,
) -> Option<ColorSpace> {
    if let Some(named) = ColorSpace::new_from_name(name) {
        let default = DeviceFamily::of_name(name).and_then(|family| defaults.get(family));
        return Some(default.map_or(named, |(space, _)| space));
    }
    let resource = resources.get_color_space(name);
    if let Some((space, _)) = resource
        .as_ref()
        .and_then(DeviceFamily::of_object)
        .and_then(|family| defaults.get(family))
    {
        return Some(space);
    }
    context.get_color_space(resources, name)
}

/// Interprets the instructions that `ops` yields, until they run out or the caller cancels. It runs
/// once per window of a content stream, between one `save_state` and its restore (see
/// `interpret_content`).
/// ASDReader patch: `indexing` makes or uses the page's content index (see `content_index`);
/// `base` is the offset of the window's first byte in the page's decoded content. Returns where to
/// continue when a group of the index is skipped.
fn interpret_window<'a>(
    ops: &mut TypedIter<'_>,
    resources: &Resources<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
    indexing: &mut crate::content_index::Indexing,
    base: u64,
    window: &[u8],
) -> Option<(u64, Vec<u8>)> {
    use crate::content_index::Op;
    // ASDReader patch (38): the resources' DefaultGray, DefaultRGB and DefaultCMYK replace the
    // device spaces that colour operators and images select (ISO 32000-2 §8.6.5.6).
    let defaults = DefaultColorSpaces::new(resources, &context.interpreter_cache.object_cache);
    let indexed = !matches!(indexing, crate::content_index::Indexing::Off);
    let recording = indexing.recording();
    let mut since_checkpoint = 0_u32;
    loop {
        let pos = base + ops.committed() as u64;
        if indexed
            && let Some(skip) = indexing.skip_to(pos, context.num_states(), context.path_is_empty(), context.clip_pending())
        {
            return Some(skip);
        }
        let Some(op) = ops.next() else { break };
        // ASDReader patch: stop when the caller cancelled (see `InterpreterSettings::cancelled`),
        // or the device asked to.
        if context.settings.cancelled.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) || device.stop_requested() {
            break;
        }
        // ASDReader patch: a moment for a device that shows the page as it is drawn.
        since_checkpoint += 1;
        if since_checkpoint >= CHECKPOINT_EVERY {
            since_checkpoint = 0;
            device.checkpoint();
        }
        let (kind, clean_before) = if recording {
            (index_op(&op), context.path_is_empty() && !context.clip_pending())
        } else {
            (Op::Other, false)
        };
        match op {
            TypedInstruction::SaveState(_) => context.save_state(),
            TypedInstruction::StrokeColorDeviceRgb(s) => {
                // ASDReader patch (38): DeviceRGB, or the DefaultRGB that replaces it.
                context.get_mut().graphics_state.stroke_cs = defaults.device(DeviceFamily::Rgb);
                context.get_mut().graphics_state.stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::StrokeColorDeviceGray(s) => {
                // ASDReader patch (38): DeviceGray, or the DefaultGray that replaces it.
                context.get_mut().graphics_state.stroke_cs = defaults.device(DeviceFamily::Gray);
                context.get_mut().graphics_state.stroke_color = smallvec![s.0.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::StrokeColorCmyk(s) => {
                // ASDReader patch (38): DeviceCMYK, or the DefaultCMYK that replaces it.
                context.get_mut().graphics_state.stroke_cs = defaults.device(DeviceFamily::Cmyk);
                context.get_mut().graphics_state.stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32(), s.3.as_f32()];
                context.get_mut().graphics_state.stroke_pattern = None;
            }
            TypedInstruction::LineWidth(w) => {
                context.get_mut().graphics_state.stroke_props.line_width = w.0.as_f32();
            }
            TypedInstruction::LineCap(c) => {
                context.get_mut().graphics_state.stroke_props.line_cap = convert_line_cap(c);
            }
            TypedInstruction::LineJoin(j) => {
                context.get_mut().graphics_state.stroke_props.line_join = convert_line_join(j);
            }
            TypedInstruction::MiterLimit(l) => {
                context.get_mut().graphics_state.stroke_props.miter_limit = l.0.as_f32();
            }
            TypedInstruction::Transform(t) => {
                context.pre_concat_transform(t);
            }
            TypedInstruction::RectPath(r) => {
                let rect = kurbo::Rect::new(
                    r.0.as_f64(),
                    r.1.as_f64(),
                    r.0.as_f64() + r.2.as_f64(),
                    r.1.as_f64() + r.3.as_f64(),
                )
                .to_path(0.1);
                context.path_mut().extend(rect);
            }
            TypedInstruction::MoveTo(m) => {
                let p = Point::new(m.0.as_f64(), m.1.as_f64());
                *(context.last_point_mut()) = p;
                *(context.sub_path_start_mut()) = p;
                context.path_mut().move_to(p);
            }
            TypedInstruction::FillPathEvenOdd(_) => {
                fill_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::FillPathNonZero(_) => {
                fill_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::FillPathNonZeroCompatibility(_) => {
                fill_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::FillAndStrokeEvenOdd(_) => {
                fill_stroke_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::FillAndStrokeNonZero(_) => {
                fill_stroke_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::CloseAndStrokePath(_) => {
                close_path(context);
                stroke_path(context, device);
            }
            TypedInstruction::CloseFillAndStrokeEvenOdd(_) => {
                close_path(context);
                fill_stroke_path(context, device, FillRule::EvenOdd);
            }
            TypedInstruction::CloseFillAndStrokeNonZero(_) => {
                close_path(context);
                fill_stroke_path(context, device, FillRule::NonZero);
            }
            TypedInstruction::NonStrokeColorDeviceGray(s) => {
                // ASDReader patch (38): DeviceGray, or the DefaultGray that replaces it.
                context.get_mut().graphics_state.none_stroke_cs = defaults.device(DeviceFamily::Gray);
                context.get_mut().graphics_state.non_stroke_color = smallvec![s.0.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::NonStrokeColorDeviceRgb(s) => {
                // ASDReader patch (38): DeviceRGB, or the DefaultRGB that replaces it.
                context.get_mut().graphics_state.none_stroke_cs = defaults.device(DeviceFamily::Rgb);
                context.get_mut().graphics_state.non_stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::NonStrokeColorCmyk(s) => {
                // ASDReader patch (38): DeviceCMYK, or the DefaultCMYK that replaces it.
                context.get_mut().graphics_state.none_stroke_cs = defaults.device(DeviceFamily::Cmyk);
                context.get_mut().graphics_state.non_stroke_color =
                    smallvec![s.0.as_f32(), s.1.as_f32(), s.2.as_f32(), s.3.as_f32()];
                context.get_mut().graphics_state.non_stroke_pattern = None;
            }
            TypedInstruction::LineTo(m) => {
                if !context.path().elements().is_empty() {
                    let last_point = *context.last_point();
                    let mut p = Point::new(m.0.as_f64(), m.1.as_f64());
                    *(context.last_point_mut()) = p;
                    if last_point == p {
                        // Add a small delta so that zero width lines can still have a round stroke.
                        p.x += 0.0001;
                    }

                    context.path_mut().line_to(p);
                }
            }
            TypedInstruction::CubicTo(c) => {
                if !context.path().elements().is_empty() {
                    let p1 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p2 = Point::new(c.2.as_f64(), c.3.as_f64());
                    let p3 = Point::new(c.4.as_f64(), c.5.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p1, p2, p3);
                }
            }
            TypedInstruction::CubicStartTo(c) => {
                if !context.path().elements().is_empty() {
                    let p1 = *context.last_point();
                    let p2 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p3 = Point::new(c.2.as_f64(), c.3.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p1, p2, p3);
                }
            }
            TypedInstruction::CubicEndTo(c) => {
                if !context.path().elements().is_empty() {
                    let p2 = Point::new(c.0.as_f64(), c.1.as_f64());
                    let p3 = Point::new(c.2.as_f64(), c.3.as_f64());

                    *(context.last_point_mut()) = p3;

                    context.path_mut().curve_to(p2, p3, p3);
                }
            }
            TypedInstruction::ClosePath(_) => {
                close_path(context);
            }
            TypedInstruction::SetGraphicsState(gs) => {
                if let Some(gs) = resources
                    .get_ext_g_state(gs.0)
                    .warn_none(&format!("failed to get extgstate {}", gs.0.as_str()))
                {
                    handle_gs(&gs, context, resources);
                }
            }
            TypedInstruction::StrokePath(_) => {
                stroke_path(context, device);
            }
            TypedInstruction::EndPath(_) => {
                apply_pending_clip(context, device);
                context.path_mut().truncate(0);
            }
            TypedInstruction::NonStrokeColor(c) => {
                let gs = &mut context.get_mut().graphics_state;
                gs.non_stroke_color = c.0.into_iter().map(|n| n.as_f32()).collect();
                gs.non_stroke_pattern = None;
            }
            TypedInstruction::StrokeColor(c) => {
                let gs = &mut context.get_mut().graphics_state;
                gs.stroke_color = c.0.into_iter().map(|n| n.as_f32()).collect();
                gs.stroke_pattern = None;
            }
            TypedInstruction::ClipNonZero(_) => {
                *(context.clip_mut()) = Some(FillRule::NonZero);
            }
            TypedInstruction::ClipEvenOdd(_) => {
                *(context.clip_mut()) = Some(FillRule::EvenOdd);
            }
            TypedInstruction::RestoreState(_) => context.restore_state(device),
            TypedInstruction::FlatnessTolerance(_) => {
                // Ignore for now.
            }
            TypedInstruction::ColorSpaceStroke(c) => {
                // ASDReader patch (38): see `named_color_space`.
                let cs = named_color_space(c.0, resources, &defaults, context)
                    .unwrap_or(ColorSpace::device_gray());

                if !cs.is_pattern() {
                    context.get_mut().graphics_state.stroke_pattern = None;
                }
                context.get_mut().graphics_state.stroke_color = cs.initial_color();
                context.get_mut().graphics_state.stroke_cs = cs;
            }
            TypedInstruction::ColorSpaceNonStroke(c) => {
                // ASDReader patch (38): see `named_color_space`.
                let cs = named_color_space(c.0, resources, &defaults, context)
                    .unwrap_or(ColorSpace::device_gray());

                if !cs.is_pattern() {
                    context.get_mut().graphics_state.non_stroke_pattern = None;
                }
                context.get_mut().graphics_state.non_stroke_color = cs.initial_color();
                context.get_mut().graphics_state.none_stroke_cs = cs;
            }
            TypedInstruction::DashPattern(p) => {
                context.get_mut().graphics_state.stroke_props.dash_offset = p.1.as_f32();
                // kurbo apparently cannot properly deal with offsets that are exactly 0.
                context.get_mut().graphics_state.stroke_props.dash_array =
                    p.0.iter::<f32>()
                        .map(|n| if n == 0.0 { 0.01 } else { n })
                        .collect();
            }
            TypedInstruction::RenderingIntent(_) => {
                // Ignore for now.
            }
            TypedInstruction::NonStrokeColorNamed(n) => {
                context.get_mut().graphics_state.non_stroke_color =
                    n.0.into_iter().map(|n| n.as_f32()).collect();
                context.get_mut().graphics_state.non_stroke_pattern = n.1.and_then(|name| {
                    resources
                        .get_pattern(name)
                        .and_then(|d| Pattern::new(d, context, resources, &defaults))
                });
            }
            TypedInstruction::StrokeColorNamed(n) => {
                context.get_mut().graphics_state.stroke_color =
                    n.0.into_iter().map(|n| n.as_f32()).collect();
                context.get_mut().graphics_state.stroke_pattern = n.1.and_then(|name| {
                    resources
                        .get_pattern(name)
                        .and_then(|d| Pattern::new(d, context, resources, &defaults))
                });
            }
            TypedInstruction::BeginMarkedContentWithProperties(bdc) => {
                // Properties can be either:
                // 1. A Name that references an entry in the Resources/Properties dictionary
                // 2. An inline dictionary with an OC key

                let mcid = dict_or_stream(bdc.1).and_then(|(props, _)| props.get::<i32>(MCID));

                let oc = bdc
                    .1
                    .clone()
                    .into_name()
                    .and_then(|name| {
                        let r = resources.properties.get_ref(name.as_ref())?;
                        let d = resources
                            .properties
                            .get::<Dict<'_>>(name)
                            .unwrap_or_default();
                        Some((d, r))
                    })
                    .or_else(|| {
                        let (props, _) = dict_or_stream(bdc.1)?;
                        let r = props.get_ref(OC)?;
                        let d = props.get::<Dict<'_>>(OC).unwrap_or_default();
                        Some((d, r))
                    });

                // ASDReader patch (51): the layer a visible `/OC` sequence puts its content on.
                let mut layer = None;
                if let Some((dict, oc_ref)) = oc {
                    context.ocg_state.begin_ocg(&dict, oc_ref.into());
                    if bdc.0.as_ref() == b"OC" && context.ocg_state.is_visible() {
                        layer = crate::ocg::layer_name(&dict);
                    }
                } else {
                    context.ocg_state.begin_marked_content();
                }

                device.begin_marked_content(bdc.0, mcid);
                if let Some(name) = layer {
                    device.marked_content_layer(&name);
                }

                // ASDReader patch (25): hand the device the sequence's `/ActualText`, from an inline
                // property list or one named in the resources' `/Properties`.
                let actual = dict_or_stream(bdc.1)
                    .map(|(props, _)| props.clone())
                    .or_else(|| {
                        let name = bdc.1.clone().into_name()?;
                        resources.properties.get::<Dict<'_>>(name)
                    })
                    .and_then(|props| props.get::<hayro_syntax::object::String<'_>>(ACTUAL_TEXT));
                if let Some(text) = actual {
                    device.actual_text(text.as_bytes());
                }
            }
            TypedInstruction::MarkedContentPointWithProperties(_) => {}
            TypedInstruction::EndMarkedContent(_) => {
                context.ocg_state.end_marked_content();
                device.end_marked_content();
            }
            TypedInstruction::MarkedContentPoint(_) => {}
            TypedInstruction::BeginMarkedContent(bmc) => {
                context.ocg_state.begin_marked_content();
                device.begin_marked_content(bmc.0, None);
            }
            TypedInstruction::BeginText(_) => {
                context.get_mut().text_state.text_matrix = Affine::IDENTITY;
                context.get_mut().text_state.text_line_matrix = Affine::IDENTITY;
            }
            TypedInstruction::SetTextMatrix(m) => {
                let m = Affine::new([
                    m.0.as_f64(),
                    m.1.as_f64(),
                    m.2.as_f64(),
                    m.3.as_f64(),
                    m.4.as_f64(),
                    m.5.as_f64(),
                ]);
                context.get_mut().text_state.text_line_matrix = m;
                context.get_mut().text_state.text_matrix = m;
            }
            TypedInstruction::EndText(_) => {
                let has_outline = context
                    .get()
                    .text_state
                    .clip_paths
                    .segments()
                    .next()
                    .is_some();

                if has_outline {
                    let clip_path = context.get().ctm * context.get().text_state.clip_paths.clone();

                    context.push_clip_path(clip_path, FillRule::NonZero, device);
                }

                context.get_mut().text_state.clip_paths.truncate(0);
            }
            TypedInstruction::TextFont(t) => {
                let name = t.0;

                // In case we are unable to resolve the font, two scenarios:
                // 1) If the font doesn't exist in the first place in the resource dictionary,
                // assume Helvetica (this seems to be what other PDF viewers do).
                // 2) In case it's `None` because we were unable to resolve the font
                // (for whatever reason), leave it as `None`. Better showing no
                // text at all than garbage text.
                let font = if let Some(font_dict) = resources.get_font(name) {
                    context.resolve_font(&font_dict)
                } else {
                    Font::new_standard(StandardFont::Helvetica, &context.settings.font_resolver)
                        .map(TextStateFont::Fallback)
                };

                context.get_mut().text_state.font_size = t.1.as_f32();
                context.get_mut().text_state.font = font;
            }
            TypedInstruction::ShowText(s) => {
                if context.get().text_state.font.is_none() {
                    // Even if no explicit font was set, we try to assume Helvetica. Acrobat
                    // seems to do the same.
                    context.get_mut().text_state.font = Font::new_standard(
                        StandardFont::Helvetica,
                        &context.settings.font_resolver,
                    )
                    .map(TextStateFont::Fallback);
                }

                text::show_text_string(context, device, resources, s.0);
            }
            TypedInstruction::ShowTexts(s) => {
                if context.get().text_state.font.is_none() {
                    // Even if no explicit font was set, we try to assume Helvetica. Acrobat
                    // seems to do the same.
                    context.get_mut().text_state.font = Font::new_standard(
                        StandardFont::Helvetica,
                        &context.settings.font_resolver,
                    )
                    .map(TextStateFont::Fallback);
                }

                for obj in s.0.iter::<Object<'_>>() {
                    match obj {
                        Object::Number(num) => {
                            context.get_mut().text_state.apply_adjustment(num.as_f32());
                        }
                        Object::String(text) => {
                            text::show_text_string(context, device, resources, &text);
                        }
                        _ => {}
                    }
                }
            }
            TypedInstruction::HorizontalScaling(h) => {
                context.get_mut().text_state.horizontal_scaling = h.0.as_f32();
            }
            TypedInstruction::TextLeading(tl) => {
                context.get_mut().text_state.leading = tl.0.as_f32();
            }
            TypedInstruction::CharacterSpacing(c) => {
                context.get_mut().text_state.char_space = c.0.as_f32();
            }
            TypedInstruction::WordSpacing(w) => {
                context.get_mut().text_state.word_space = w.0.as_f32();
            }
            TypedInstruction::NextLine(n) => {
                let (tx, ty) = (n.0.as_f64(), n.1.as_f64());
                text::next_line(context, tx, ty);
            }
            TypedInstruction::NextLineUsingLeading(_) => {
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
            }
            TypedInstruction::NextLineAndShowText(n) => {
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
                text::show_text_string(context, device, resources, n.0);
            }
            TypedInstruction::TextRenderingMode(r) => {
                let mode = match r.0.as_i64() {
                    0 => TextRenderingMode::Fill,
                    1 => TextRenderingMode::Stroke,
                    2 => TextRenderingMode::FillStroke,
                    3 => TextRenderingMode::Invisible,
                    4 => TextRenderingMode::FillAndClip,
                    5 => TextRenderingMode::StrokeAndClip,
                    6 => TextRenderingMode::FillAndStrokeAndClip,
                    7 => TextRenderingMode::Clip,
                    _ => {
                        warn!("unknown text rendering mode {}", r.0.as_i64());

                        TextRenderingMode::Fill
                    }
                };

                context.get_mut().text_state.render_mode = mode;
            }
            TypedInstruction::NextLineAndSetLeading(n) => {
                let (tx, ty) = (n.0.as_f64(), n.1.as_f64());
                context.get_mut().text_state.leading = -ty as f32;
                text::next_line(context, tx, ty);
            }
            TypedInstruction::ShapeGlyph(_) => {}
            TypedInstruction::XObject(x) => {
                let cache = context.interpreter_cache.object_cache.clone();
                // ASDReader patch: decoded images are cached per quality (preview or final).
                let decoded_cache = context
                    .interpreter_cache
                    .decoded_cache
                    .for_render(context.settings.preview);
                let transfer_function = context.get().graphics_state.transfer_function.clone();
                if let Some(x_object) = resources.get_x_object(x.0).and_then(|s| {
                    XObject::new(
                        &s,
                        &context.settings.warning_sink,
                        &cache,
                        Some(&decoded_cache),
                        transfer_function.clone(),
                        // ASDReader patch (38): an image in a device space draws in its default.
                        &defaults,
                    )
                }) {
                    draw_xobject(&x_object, resources, context, device);
                }
            }
            TypedInstruction::InlineImage(i) => {
                let warning_sink = context.settings.warning_sink.clone();
                let transfer_function = context.get().graphics_state.transfer_function.clone();
                let cache = context.interpreter_cache.object_cache.clone();
                // ASDReader patch: inline images are not cached (see `DecodedImageKey`).
                // ASDReader patch (38): an inline image in a device space, named directly or by a
                // resource, draws in its default.
                if let Some(x_object) = ImageXObject::new(
                    i.0,
                    |name| named_color_space(name, resources, &defaults, context),
                    &warning_sink,
                    &cache,
                    None,
                    false,
                    transfer_function,
                    Some(&defaults),
                ) {
                    draw_image_xobject(&x_object, context, device);
                }
            }
            TypedInstruction::TextRise(t) => {
                context.get_mut().text_state.rise = t.0.as_f32();
            }
            TypedInstruction::Shading(s) => {
                if !context.ocg_state.is_visible() {
                    continue;
                }

                let transfer_function = context.get().graphics_state.transfer_function.clone();

                if let Some(sp) = resources
                    .get_shading(s.0)
                    .and_then(|o| {
                        let (dict, stream) = dict_or_stream(&o)?;
                        // ASDReader patch (38): a shading in a device space draws in its default.
                        Shading::new(
                            dict,
                            stream,
                            &context.interpreter_cache.object_cache,
                            &defaults,
                        )
                    })
                    .map(|s| {
                        Pattern::Shading(ShadingPattern {
                            shading: Arc::new(s),
                            matrix: Affine::IDENTITY,
                            opacity: context.get().graphics_state.non_stroke_alpha,
                            transfer_function: transfer_function.clone(),
                        })
                    })
                {
                    context.save_state();
                    context.push_root_transform();
                    let st = context.get_mut();
                    st.graphics_state.non_stroke_pattern = Some(sp);
                    st.graphics_state.none_stroke_cs = ColorSpace::pattern();

                    device.set_soft_mask(st.graphics_state.soft_mask.clone());
                    device.set_blend_mode(st.graphics_state.blend_mode);

                    let bbox = context.bbox().to_path(0.1);
                    let inverted_bbox = context.get().ctm.inverse() * bbox;
                    fill_path_impl(context, device, FillRule::NonZero, Some(&inverted_bbox));

                    context.pop_root_transform();
                    context.restore_state(device);
                } else {
                    warn!("failed to process shading");
                }
            }
            TypedInstruction::BeginCompatibility(_) => {}
            TypedInstruction::EndCompatibility(_) => {}
            TypedInstruction::ColorGlyph(_) => {}
            TypedInstruction::ShowTextWithParameters(t) => {
                context.get_mut().text_state.word_space = t.0.as_f32();
                context.get_mut().text_state.char_space = t.1.as_f32();
                text::next_line(context, 0.0, -context.get().text_state.leading as f64);
                text::show_text_string(context, device, resources, t.2);
            }
            _ => {
                warn!("failed to read an operator");
            }
        }
        if recording {
            let end = base + ops.committed() as u64;
            let from = usize::try_from(pos - base).unwrap_or(usize::MAX);
            let to = usize::try_from(end - base).unwrap_or(usize::MAX);
            let bytes = window.get(from..to).unwrap_or(&[]);
            indexing.after(kind, pos, end, bytes, context.num_states(), clean_before, context.path_is_empty());
        }
    }
    None
}

/// ASDReader patch: content operators between two `Device::checkpoint` calls.
const CHECKPOINT_EVERY: u32 = 2048;

/// ASDReader patch: what `op` is to the content index (see `content_index`).
fn index_op(op: &TypedInstruction<'_, '_>) -> crate::content_index::Op {
    use crate::content_index::{Key, Op, Sets};
    use TypedInstruction as I;
    match op {
        I::SaveState(_) => Op::Save,
        I::RestoreState(_) => Op::Restore,
        I::BeginText(_) => Op::BeginText,
        I::EndText(_) => Op::EndText,
        I::BeginMarkedContent(_) | I::BeginMarkedContentWithProperties(_) => Op::BeginMarked,
        I::EndMarkedContent(_) => Op::EndMarked,
        I::MarkedContentPoint(_) | I::MarkedContentPointWithProperties(_) => Op::Nothing,
        I::MoveTo(_)
        | I::LineTo(_)
        | I::CubicTo(_)
        | I::CubicStartTo(_)
        | I::CubicEndTo(_)
        | I::ClosePath(_)
        | I::RectPath(_) => Op::Build,
        I::ClipNonZero(_) | I::ClipEvenOdd(_) => Op::Clip,
        I::StrokePath(_)
        | I::CloseAndStrokePath(_)
        | I::FillPathNonZero(_)
        | I::FillPathNonZeroCompatibility(_)
        | I::FillPathEvenOdd(_)
        | I::FillAndStrokeNonZero(_)
        | I::FillAndStrokeEvenOdd(_)
        | I::CloseFillAndStrokeNonZero(_)
        | I::CloseFillAndStrokeEvenOdd(_)
        | I::EndPath(_) => Op::Paint,
        I::Transform(_) => Op::State(Key::Cm, Sets::Other),
        I::LineWidth(_) => Op::State(Key::LineWidth, Sets::Other),
        I::LineCap(_) => Op::State(Key::LineCap, Sets::Other),
        I::LineJoin(_) => Op::State(Key::LineJoin, Sets::Other),
        I::MiterLimit(_) => Op::State(Key::MiterLimit, Sets::Other),
        I::DashPattern(_) => Op::State(Key::Dash, Sets::Other),
        I::RenderingIntent(_) => Op::State(Key::Intent, Sets::Other),
        I::FlatnessTolerance(_) => Op::State(Key::Flatness, Sets::Other),
        I::SetGraphicsState(_) => Op::State(Key::Gs, Sets::Other),
        I::ColorSpaceStroke(_) => Op::State(Key::StrokeColor, Sets::Space),
        I::ColorSpaceNonStroke(_) => Op::State(Key::FillColor, Sets::Space),
        I::StrokeColorDeviceGray(_) | I::StrokeColorDeviceRgb(_) | I::StrokeColorCmyk(_) => {
            Op::State(Key::StrokeColor, Sets::Space)
        }
        I::NonStrokeColorDeviceGray(_) | I::NonStrokeColorDeviceRgb(_) | I::NonStrokeColorCmyk(_) => {
            Op::State(Key::FillColor, Sets::Space)
        }
        I::StrokeColor(_) | I::StrokeColorNamed(_) => Op::State(Key::StrokeColor, Sets::Colour),
        I::NonStrokeColor(_) | I::NonStrokeColorNamed(_) => Op::State(Key::FillColor, Sets::Colour),
        I::CharacterSpacing(_) => Op::State(Key::Text(0), Sets::Other),
        I::WordSpacing(_) => Op::State(Key::Text(1), Sets::Other),
        I::HorizontalScaling(_) => Op::State(Key::Text(2), Sets::Other),
        I::TextLeading(_) => Op::State(Key::Text(3), Sets::Other),
        I::TextFont(_) => Op::State(Key::Text(4), Sets::Other),
        I::TextRenderingMode(_) => Op::State(Key::Text(5), Sets::Other),
        I::TextRise(_) => Op::State(Key::Text(6), Sets::Other),
        I::XObject(_) | I::Shading(_) => Op::Draw,
        I::NextLine(_)
        | I::NextLineAndSetLeading(_)
        | I::SetTextMatrix(_)
        | I::NextLineUsingLeading(_)
        | I::ShowText(_)
        | I::NextLineAndShowText(_)
        | I::ShowTextWithParameters(_)
        | I::ShowTexts(_) => Op::TextObject,
        _ => Op::Other,
    }
}

/// Interpret the instructions from `ops` and render them into the device.
pub fn interpret<'a>(
    mut ops: TypedIter<'_>,
    resources: &Resources<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
) {
    let num_states = context.num_states();

    context.save_state();

    interpret_window(
        &mut ops,
        resources,
        context,
        device,
        &mut crate::content_index::Indexing::Off,
        0,
        &[],
    );

    while context.num_states() > num_states {
        context.restore_state(device);
    }
    // ASDReader patch (18): say that content was skipped, so a truncated page isn't a clean render.
    if crate::context::content_was_truncated() {
        (context.settings.warning_sink)(InterpreterWarning::ContentTruncated);
    }
}

/// ASDReader patch: the bytes of a window of page content. A window grows only while an instruction
/// at its end is incomplete.
const CONTENT_WINDOW: usize = 64 << 10;

/// ASDReader patch: interprets the content of a page a window at a time, so the decoded content is
/// never all in memory (see `ContentReader`). A window ends at an instruction that is not complete
/// yet: its bytes are kept, and the next window starts with them.
fn interpret_content<'a>(
    content: &mut ContentReader<'_>,
    resources: &Resources<'a>,
    context: &mut Context<'a>,
    device: &mut impl Device<'a>,
    indexing: &mut crate::content_index::Indexing,
) {
    let num_states = context.num_states();

    context.save_state();
    indexing.set_level(context.num_states());

    let mut window = Vec::with_capacity(CONTENT_WINDOW);
    let mut exhausted = false;
    let mut cut = false;
    // ASDReader patch: the offset of `window[0]` in the decoded content.
    let mut base = 0_u64;
    loop {
        let (committed, needs_more, skip) = {
            let mut ops = TypedIter::new_window(&window, exhausted);
            let skip = interpret_window(&mut ops, resources, context, device, indexing, base, &window);
            (ops.committed(), ops.needs_more(), skip)
        };
        // ASDReader patch: a group of the content index that can't reach the area being
        // rendered: its bytes are passed over unread.
        if let Some((end, replay)) = skip {
            // What the group set that outlasts it, so that what follows is drawn as before.
            if !replay.is_empty() {
                let mut replayed = TypedIter::new(&replay);
                interpret_window(&mut replayed, resources, context, device, &mut crate::content_index::Indexing::Off, 0, &[]);
            }
            let ahead = usize::try_from(end.saturating_sub(base)).unwrap_or(usize::MAX);
            if ahead <= window.len() {
                window.drain(..ahead);
            } else {
                let rest = ahead - window.len();
                window.clear();
                if content.skip(rest) < rest {
                    exhausted = true;
                }
            }
            base = end;
            if !exhausted && window.len() < CONTENT_WINDOW {
                let want = CONTENT_WINDOW - window.len();
                if content.read_into(&mut window, want) < want {
                    exhausted = true;
                }
            }
            if context.settings.cancelled.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
                break;
            }
            continue;
        }
        window.drain(..committed);
        base += committed as u64;

        let cancelled = context
            .settings
            .cancelled
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed));
        if !needs_more || cancelled || exhausted {
            break;
        }
        // A window that is still incomplete at its end waits for more of the stream. It doubles
        // while it waits, so one long instruction costs time in proportion to its length, and it
        // stops growing at `MAX_DECODED_STREAM`, the most any one stream may decode to.
        if window.len() >= MAX_DECODED_STREAM {
            cut = true;
            break;
        }
        let want = window.len().max(CONTENT_WINDOW);
        if content.read_into(&mut window, want) == 0 {
            exhausted = true;
        }
    }

    while context.num_states() > num_states {
        context.restore_state(device);
    }
    // ASDReader patch (18): say that content was skipped, so a truncated page isn't a clean render.
    if cut || content.truncated() || crate::context::content_was_truncated() {
        (context.settings.warning_sink)(InterpreterWarning::ContentTruncated);
    }
}
