//! ASDReader patch: an index of a page's content stream that lets a render of part of the page
//! skip the content that cannot reach it, as other readers keep a page's objects once they have
//! been parsed and draw only those in view.
//!
//! The index is made while a whole page is rendered ([`ContentIndexMode::Record`]). It divides the
//! page's content into *items*: a `q … Q` block, a marked-content sequence (`BDC … EMC`), a text
//! object (`BT … ET`), one path with the operator that paints it, a `Do` or `sh`, or one
//! instruction that sets part of the graphics state (colours, line width, `cm`, `gs`, the text
//! state…). Items that follow each other at the same nesting level are merged into *groups*
//! while their boxes stay tight. Each group records:
//!
//! - its bytes in the decoded content and the state level it starts at;
//! - the box, in page space, of everything it draws, with a margin in device pixels for
//!   anti-aliasing, the one-pixel minimum width of strokes and their miters;
//! - its *replay*: the instructions whose effect outlasts the group (the last colour, line width,
//!   `gs` and so on that it set, every `cm` outside a `q … Q`), in the order they last took effect.
//!
//! A later render ([`ContentIndexMode::Use`]) passes over the bytes of every group whose box
//! misses the area it draws, without reading them, and runs the group's replay instead, so what
//! follows is drawn in the state it would have had. A group that draws nothing (content of a
//! hidden layer) is passed over in every render.
//!
//! What the index can't account for is never passed over: an item that draws an image, a Type 3
//! glyph, a transparency group or a soft mask, or sets a clip that outlasts it, or any
//! instruction outside the list above; a text object is passed over whole or not at all. Content
//! whose nesting doesn't match (`BDC q … EMC Q`) gets no index. The index belongs to one document,
//! one page and one set of layer settings; the caller keeps it with them.

use crate::device::Device;
use crate::font::Glyph;
use crate::soft_mask::SoftMask;
use crate::{BlendMode, CacheKey, ClipPath, GlyphDrawMode, Image, Paint, PathDrawMode};
use kurbo::{Affine, BezPath, Rect, Shape};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use smallvec::SmallVec;
use std::sync::{Arc, Mutex};

/// One instruction's bytes; most fit inline.
type Bytes = SmallVec<[u8; 40]>;

/// What a page render does with a [`ContentIndex`].
#[derive(Clone, Default)]
pub enum ContentIndexMode {
    /// Neither make nor use one.
    #[default]
    Off,
    /// Make the index of the page while rendering it whole and put it in the slot (left empty
    /// when the render was cut short or cancelled). Only for a final render of the whole page:
    /// a preview skips images, and a tile's interpreter culls what lies outside it.
    Record(Arc<Mutex<Option<Arc<ContentIndex>>>>),
    /// Pass over the groups of the index that miss the area being rendered.
    Use(Arc<ContentIndex>),
}

/// See the module documentation.
#[derive(Debug, Default)]
pub struct ContentIndex {
    groups: Vec<Group>,
}

#[derive(Debug, Clone)]
struct Group {
    /// Offset in the page's decoded content of the group's first byte (the whitespace before it
    /// included), and just past its last instruction.
    start: u64,
    end: u64,
    /// The state level (saved states) the group starts and ends at.
    level: usize,
    /// What the group draws, in page space; `None` when it draws nothing.
    bounds: Option<Rect>,
    /// Device pixels the group's paint may reach past `bounds` at any scale.
    margin: f64,
    /// Items in the group.
    items: u32,
    /// The instructions to run in place of the group (see the module documentation).
    replay: Replay,
}

impl ContentIndex {
    /// How many groups the index has (for tests and diagnostics).
    pub fn groups(&self) -> usize {
        self.groups.len()
    }

    /// How many of the groups draw nothing (for tests and diagnostics).
    pub fn empty_groups(&self) -> usize {
        self.groups.iter().filter(|g| g.bounds.is_none()).count()
    }
}

/// The most groups an index keeps; a page with more is rendered without one.
const MAX_GROUPS: usize = 1 << 19;
/// The most items merged into one group.
const MAX_ITEMS: u32 = 4096;
/// The most replay entries a group or a sequence keeps; one with more is never passed over.
const MAX_REPLAY: usize = 32;
/// The first items of a frame (`q … Q`, marked content), which are indexed only with the frame.
const SMALL_FRAME: u32 = 16;
/// Device pixels every bound is grown by: anti-aliasing and the half pixel a hairline moves to
/// the pixel grid.
const SLACK: f64 = 2.0;

/// Which part of the state a replay entry sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    StrokeColor,
    FillColor,
    LineWidth,
    LineCap,
    LineJoin,
    MiterLimit,
    Dash,
    Intent,
    Flatness,
    /// `gs`, by the bytes of the instruction (the same name sets the same entries).
    Gs,
    /// `cm`: every one counts, in order.
    Cm,
    /// One of the text state operators, by its index.
    Text(u8),
}

/// What an instruction sets: for colours, whether it chooses the colour space as well
/// (`RG`, `CS`…) or only the colour in the current space (`SC`, `SCN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sets {
    Space,
    Colour,
    Other,
}

#[derive(Debug, Clone, Default)]
struct Replay {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone)]
struct Entry {
    key: Key,
    /// For colours: the instruction that chose the colour space, if the replayed content did.
    space: Option<Bytes>,
    /// The instruction, or for colours the one that set the colour last.
    bytes: Option<Bytes>,
}

impl Replay {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records that `bytes` set `key` after everything already in the replay.
    fn set(&mut self, key: Key, sets: Sets, bytes: &[u8]) {
        let bytes = Bytes::from_slice(bytes);
        let mut entry = Entry { key, space: None, bytes: None };
        match sets {
            Sets::Space => entry.space = Some(bytes),
            Sets::Colour | Sets::Other => entry.bytes = Some(bytes),
        }
        self.apply(entry);
    }

    /// Adds `entry`, which took effect after everything already in the replay.
    fn apply(&mut self, mut entry: Entry) {
        if entry.key != Key::Cm && entry.key != Key::Gs {
            if let Some(i) = self.entries.iter().position(|e| e.key == entry.key) {
                let old = self.entries.remove(i);
                // A colour alone is set in the space chosen before it.
                if entry.space.is_none() && matches!(entry.key, Key::StrokeColor | Key::FillColor) {
                    entry.space = old.space;
                }
            }
        } else if entry.key == Key::Gs {
            let same = |e: &Entry| e.key == Key::Gs && e.bytes == entry.bytes;
            if let Some(i) = self.entries.iter().position(same) {
                self.entries.remove(i);
            }
        }
        self.entries.push(entry);
    }

    /// Adds what `later` sets, which took effect after this replay.
    fn extend(&mut self, later: &Self) {
        for entry in &later.entries {
            self.apply(entry.clone());
        }
    }

    /// The instructions to run, separated by newlines.
    pub(crate) fn program(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for e in &self.entries {
            for part in [&e.space, &e.bytes].into_iter().flatten() {
                out.extend_from_slice(part);
                out.push(b'\n');
            }
        }
        out
    }
}

/// Bounds collected for the instruction being run, by the [`BoundsDevice`].
#[derive(Default, Clone, Copy)]
pub(crate) struct Collected {
    bounds: Option<Rect>,
    margin: f64,
    /// Drew something whose bounds aren't known.
    unbounded: bool,
    /// Set a clip.
    clipped: bool,
}

impl Collected {
    fn add(&mut self, rect: Rect, margin: f64) {
        if !(rect.x0.is_finite() && rect.y0.is_finite() && rect.x1.is_finite() && rect.y1.is_finite())
            || !margin.is_finite()
        {
            self.unbounded = true;
            return;
        }
        self.bounds = Some(self.bounds.map_or(rect, |b| b.union(rect)));
        self.margin = self.margin.max(margin);
    }

    fn absorb(&mut self, other: &Self) {
        self.unbounded |= other.unbounded;
        self.clipped |= other.clipped;
        if let Some(b) = other.bounds {
            self.bounds = Some(self.bounds.map_or(b, |a| a.union(b)));
        }
        self.margin = self.margin.max(other.margin);
    }
}

/// What the device collected for the instruction being run.
pub(crate) type Collecting = Rc<Cell<Collected>>;

/// The interpreter's side of making or using an index, for one page's top-level content.
pub(crate) enum Indexing {
    Off,
    Record(Box<Recorder>),
    Use(Skipper),
}

/// What one instruction is to the index.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Save,
    Restore,
    BeginText,
    EndText,
    BeginMarked,
    EndMarked,
    /// Builds the current path (`m`, `l`, `re`…).
    Build,
    /// `W`, `W*`.
    Clip,
    /// Paints or ends the current path.
    Paint,
    /// Sets part of the state, as a replay entry.
    State(Key, Sets),
    /// Draws, with no effect that outlasts it (`Do`, `sh`).
    Draw,
    /// Shows text or moves within a text object.
    TextObject,
    /// Does nothing to what is drawn (`MP`, `DP`).
    Nothing,
    /// Anything else: never passed over.
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Root,
    Save,
    Marked,
    Text,
}

/// A nesting level of the content: the page itself, or an open `q`, `BDC`/`BMC` or `BT`.
struct Frame {
    kind: FrameKind,
    start: u64,
    /// The state level inside the frame.
    level: usize,
    /// Opened with no path under construction and no clip pending.
    clean: bool,
    /// What the frame's content drew.
    collected: Collected,
    /// What the frame's content set that outlasts it (for `q` frames, nothing does).
    replay: Replay,
    /// Content whose effect outlasts it and isn't in `replay`.
    lasting: bool,
    /// The group the next item at this level may join.
    last_group: Option<usize>,
    /// The path under construction: where it started and what its building has drawn (nothing).
    path: Option<PathItem>,
    /// How many items the frame has had.
    items: u32,
}

struct PathItem {
    start: u64,
    collected: Collected,
    clipped: bool,
}

pub(crate) struct Recorder {
    collecting: Collecting,
    frames: Vec<Frame>,
    groups: Vec<Group>,
    /// Set when the index can't be trusted (mismatched nesting, too many groups).
    failed: bool,
    /// Area under which two boxes are merged regardless of how loose that makes the group.
    small_area: f64,
}

pub(crate) struct Skipper {
    index: Arc<ContentIndex>,
    next: usize,
    /// The area being rendered, in page space.
    view: Rect,
    /// Page-space length of one device pixel (the largest, for a page drawn anisotropically).
    pixel: f64,
}

/// One finished item, offered to its frame.
struct Item {
    start: u64,
    end: u64,
    collected: Collected,
    replay: Replay,
    /// Has an effect that outlasts it and isn't in `replay`.
    lasting: bool,
}

impl Indexing {
    /// The indexing of a page render with `mode`. `to_page` maps device space to page space,
    /// `canvas` is the device area being rendered and `page_box` the page in page space.
    pub(crate) fn new(mode: ContentIndexMode, to_page: Affine, canvas: Rect, page_box: Rect) -> (Self, Option<Collecting>) {
        match mode {
            ContentIndexMode::Off => (Self::Off, None),
            ContentIndexMode::Record(_) => {
                let collecting = Collecting::default();
                let side = page_box.width().abs().min(page_box.height().abs());
                (
                    Self::Record(Box::new(Recorder {
                        collecting: collecting.clone(),
                        frames: Vec::new(),
                        groups: Vec::new(),
                        failed: false,
                        small_area: (side / 64.0).powi(2),
                    })),
                    Some(collecting),
                )
            }
            ContentIndexMode::Use(index) => {
                let view = to_page.transform_rect_bbox(canvas);
                let pixel = max_scale(&to_page);
                if !(pixel.is_finite() && view.x0.is_finite() && view.x1.is_finite()) {
                    return (Self::Off, None);
                }
                (Self::Use(Skipper { index, next: 0, view, pixel }), None)
            }
        }
    }

    /// Whether this render records instructions (and needs their bytes).
    pub(crate) fn recording(&self) -> bool {
        matches!(self, Self::Record(_))
    }

    /// Set the page's own state level, once the content's outer state is saved.
    pub(crate) fn set_level(&mut self, level: usize) {
        if let Self::Record(r) = self {
            r.frames.push(Frame::new(FrameKind::Root, 0, level, true));
        }
    }

    /// Where to continue, and what to run first, when the content at `pos` (the start of the
    /// next instruction) begins a group that misses the area being rendered. `level` is the state
    /// level there.
    pub(crate) fn skip_to(&mut self, pos: u64, level: usize, path_empty: bool, clip_pending: bool) -> Option<(u64, Vec<u8>)> {
        let Self::Use(s) = self else { return None };
        let groups = &s.index.groups;
        while groups.get(s.next).is_some_and(|g| g.start < pos) {
            s.next += 1;
        }
        if !path_empty || clip_pending {
            return None;
        }
        let g = groups.get(s.next).filter(|g| g.start == pos && g.level == level)?;
        let visible = g.bounds.is_some_and(|b| {
            let grow = g.margin * s.pixel;
            b.inflate(grow, grow).overlaps(s.view)
        });
        if visible {
            return None;
        }
        s.next += 1;
        Some((g.end, if g.replay.is_empty() { Vec::new() } else { g.replay.program() }))
    }

    /// Called after an instruction ran. `start` is where it began and `end` just past it,
    /// `bytes` the instruction itself, `level` the state level after it, and `path_empty` and
    /// `clip_pending` the path's state before (`*_before`) and after it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn after(
        &mut self,
        op: Op,
        start: u64,
        end: u64,
        bytes: &[u8],
        level: usize,
        clean_before: bool,
        path_empty: bool,
    ) {
        let Self::Record(r) = self else { return };
        if r.failed {
            return;
        }
        let collected = r.collecting.take();
        r.step(op, start, end, bytes, level, clean_before, path_empty, collected);
    }

    /// The finished index of a recording, if it can be used.
    pub(crate) fn finish(self) -> Option<ContentIndex> {
        let Self::Record(mut r) = self else { return None };
        if r.failed || r.groups.len() > MAX_GROUPS {
            return None;
        }
        // Inner items end, and are grouped, before the items around them.
        r.groups.sort_by_key(|g| g.start);
        Some(ContentIndex { groups: r.groups })
    }
}

impl Frame {
    fn new(kind: FrameKind, start: u64, level: usize, clean: bool) -> Self {
        Self {
            kind,
            start,
            level,
            clean,
            collected: Collected::default(),
            replay: Replay::default(),
            lasting: false,
            last_group: None,
            path: None,
            items: 0,
        }
    }
}

impl Recorder {
    #[allow(clippy::too_many_arguments)]
    fn step(&mut self, op: Op, start: u64, end: u64, bytes: &[u8], level: usize, clean_before: bool, path_empty: bool, collected: Collected) {
        // A path under construction: building goes on, painting ends it, anything else spoils it.
        if let Some(frame) = self.frames.last_mut()
            && let Some(path) = frame.path.as_mut()
        {
            match op {
                Op::Build => {
                    path.collected.absorb(&collected);
                    return;
                }
                Op::Clip => {
                    path.clipped = true;
                    return;
                }
                Op::Paint => {
                    let Some(mut path) = frame.path.take() else { return };
                    path.collected.absorb(&collected);
                    let lasting = path.clipped || path.collected.clipped || !path_empty;
                    let item = Item { start: path.start, end, collected: path.collected, replay: Replay::default(), lasting };
                    self.offer(item);
                    return;
                }
                _ => {
                    frame.path = None;
                    frame.lasting = true;
                    frame.last_group = None;
                }
            }
        }
        match op {
            Op::Build | Op::Clip | Op::Paint => {
                let Some(frame) = self.frames.last_mut() else { return };
                if op == Op::Build && clean_before {
                    frame.path = Some(PathItem { start, collected, clipped: false });
                } else {
                    // A path started dirty, or a clip or paint with no path: kept, never skipped.
                    frame.collected.absorb(&collected);
                    frame.lasting = true;
                    frame.last_group = None;
                }
            }
            Op::Save => {
                let Some(parent) = self.frames.last() else { return };
                let clean = clean_before && parent.kind != FrameKind::Text;
                self.frames.push(Frame::new(FrameKind::Save, start, level, clean));
            }
            Op::BeginMarked | Op::BeginText => {
                let Some(parent) = self.frames.last() else { return };
                let kind = if op == Op::BeginText { FrameKind::Text } else { FrameKind::Marked };
                let clean = clean_before && parent.kind != FrameKind::Text;
                self.frames.push(Frame::new(kind, start, level, clean));
            }
            Op::Restore | Op::EndMarked | Op::EndText => {
                let kind = match op {
                    Op::Restore => FrameKind::Save,
                    Op::EndMarked => FrameKind::Marked,
                    _ => FrameKind::Text,
                };
                let matches = self.frames.last().is_some_and(|f| f.kind == kind && (kind != FrameKind::Save || level + 1 == f.level));
                if !matches {
                    // A `Q` with no `q` of its own at the page's level (files do start with one)
                    // returns to the state the page began with, or does nothing at the bottom of
                    // the stack: the page goes on from there at that level, and nothing before it
                    // joins anything after it. Any other mismatch leaves the nesting unknown.
                    match self.frames.last_mut() {
                        Some(root) if op == Op::Restore && root.kind == FrameKind::Root && level <= root.level => {
                            root.level = level;
                            root.lasting = true;
                            root.last_group = None;
                            root.path = None;
                        }
                        _ => self.failed = true,
                    }
                    return;
                }
                let Some(mut frame) = self.frames.pop() else { return };
                frame.collected.absorb(&collected);
                let restored = kind == FrameKind::Save;
                let lasting = !frame.clean
                    || !path_empty
                    || frame.path.is_some()
                    || (!restored && (frame.lasting || frame.collected.clipped || frame.replay.entries.len() > MAX_REPLAY));
                let mut collected = frame.collected;
                if restored {
                    // `Q` undoes the frame's clips.
                    collected.clipped = false;
                }
                let replay = if restored { Replay::default() } else { frame.replay };
                self.offer(Item { start: frame.start, end, collected, replay, lasting });
            }
            Op::State(key, sets) => {
                let Some(frame) = self.frames.last_mut() else { return };
                if frame.kind == FrameKind::Text {
                    frame.replay.set(key, sets, bytes);
                    return;
                }
                // The first items of a `q` frame make no group, and `Q` undoes what they set.
                if frame.kind == FrameKind::Save && frame.items < SMALL_FRAME {
                    frame.items += 1;
                    frame.lasting |= !clean_before;
                    return;
                }
                let mut replay = Replay::default();
                replay.set(key, sets, bytes);
                let lasting = !clean_before;
                self.offer(Item { start, end, collected, replay, lasting });
            }
            Op::Draw | Op::Nothing => {
                let Some(frame) = self.frames.last_mut() else { return };
                if frame.kind == FrameKind::Text {
                    frame.collected.absorb(&collected);
                    return;
                }
                let lasting = !clean_before || collected.clipped;
                self.offer(Item { start, end, collected, replay: Replay::default(), lasting });
            }
            Op::TextObject => {
                let Some(frame) = self.frames.last_mut() else { return };
                frame.collected.absorb(&collected);
                if frame.kind != FrameKind::Text {
                    frame.lasting = true;
                    frame.last_group = None;
                }
            }
            Op::Other => {
                let Some(frame) = self.frames.last_mut() else { return };
                frame.collected.absorb(&collected);
                frame.lasting = true;
                frame.last_group = None;
            }
        }
    }

    /// Adds a finished `item` to the frame it is in: to that frame's own bounds and replay, and,
    /// unless the frame is a text object, to the index.
    fn offer(&mut self, item: Item) {
        let Some(frame) = self.frames.last_mut() else { return };
        frame.items = frame.items.saturating_add(1);
        frame.collected.absorb(&item.collected);
        // `Q` undoes what a `q` frame's content set; the page's own replay is never used.
        if matches!(frame.kind, FrameKind::Marked | FrameKind::Text) {
            frame.replay.extend(&item.replay);
        }
        frame.lasting |= item.lasting;
        // A text object is passed over whole or not at all, and the first items of a frame are
        // only indexed with it: most frames are small (a `q … Q` around every stroke), and
        // indexing their insides cost more than it saved.
        if frame.kind == FrameKind::Text || (frame.kind != FrameKind::Root && frame.items <= SMALL_FRAME) {
            return;
        }
        let level = frame.level;
        if item.lasting || item.collected.unbounded || item.replay.entries.len() > MAX_REPLAY {
            frame.last_group = None;
            return;
        }
        let bounds = item.collected.bounds;
        if let Some(last) = frame.last_group.and_then(|i| self.groups.get_mut(i))
            && last.end == item.start
            && last.items < MAX_ITEMS
        {
            let merged = match (last.bounds, bounds) {
                (Some(a), Some(b)) => {
                    let u = a.union(b);
                    (u.area() <= 2.0 * (a.area() + b.area()) + self.small_area).then_some(Some(u))
                }
                (a, b) => Some(a.or(b)),
            };
            let fits = last.replay.entries.len() + item.replay.entries.len() <= MAX_REPLAY;
            if let Some(bounds) = merged
                && fits
            {
                last.end = item.end;
                last.bounds = bounds;
                last.margin = last.margin.max(item.collected.margin);
                last.items += 1;
                last.replay.extend(&item.replay);
                return;
            }
        }
        if self.groups.len() >= MAX_GROUPS {
            self.failed = true;
            return;
        }
        self.groups.push(Group {
            start: item.start,
            end: item.end,
            level,
            bounds,
            margin: item.collected.margin,
            items: 1,
            replay: item.replay,
        });
        if let Some(frame) = self.frames.last_mut() {
            frame.last_group = Some(self.groups.len() - 1);
        }
    }
}

/// The largest factor by which `t` stretches a length.
fn max_scale(t: &Affine) -> f64 {
    let [a, b, c, d, _, _] = t.as_coeffs();
    a.hypot(b).max(c.hypot(d)) * core::f64::consts::SQRT_2
}

/// A device that passes everything on to `inner` and collects the page-space bounds of what is
/// drawn, for the block being recorded.
pub(crate) struct BoundsDevice<'d, D> {
    pub(crate) inner: &'d mut D,
    pub(crate) collecting: Collecting,
    /// Device space to page space.
    pub(crate) to_page: Affine,
    pub(crate) glyph_bounds: HashMap<u128, Option<Rect>>,
}

impl<D> BoundsDevice<'_, D> {
    fn add_path(&self, path: &BezPath, transform: Affine, mode: &PathDrawMode) {
        let to_page = self.to_page * transform;
        let rect = to_page.transform_rect_bbox(path.bounding_box());
        let (rect, margin) = match mode {
            PathDrawMode::Fill(_) => (rect, SLACK),
            PathDrawMode::Stroke(s) => {
                let Some(miter) = miter_factor(s.line_join, s.miter_limit) else {
                    self.unbounded();
                    return;
                };
                let reach = f64::from(s.line_width.abs()) / 2.0 * miter * max_scale(&to_page);
                (rect.inflate(reach, reach), 0.5 * miter + SLACK)
            }
        };
        let mut c = self.collecting.get();
        c.add(rect, margin);
        self.collecting.set(c);
    }

    fn unbounded(&self) {
        let mut c = self.collecting.get();
        c.unbounded = true;
        self.collecting.set(c);
    }
}

/// How far past half the width a stroke's joins and caps reach, as a multiple of half the width.
fn miter_factor(join: kurbo::Join, limit: f32) -> Option<f64> {
    let square = core::f64::consts::SQRT_2;
    match join {
        kurbo::Join::Miter => {
            let limit = f64::from(limit.abs());
            limit.is_finite().then_some(limit.max(square))
        }
        _ => Some(square),
    }
}

impl<'a, D: Device<'a>> Device<'a> for BoundsDevice<'_, D> {
    fn set_soft_mask(&mut self, mask: Option<SoftMask<'a>>) {
        if mask.is_some() {
            self.unbounded();
        }
        self.inner.set_soft_mask(mask);
    }

    fn set_blend_mode(&mut self, blend_mode: BlendMode) {
        self.inner.set_blend_mode(blend_mode);
    }

    fn draw_path(&mut self, path: &BezPath, transform: Affine, paint: &Paint<'a>, mode: &PathDrawMode) {
        self.add_path(path, transform, mode);
        self.inner.draw_path(path, transform, paint, mode);
    }

    fn push_clip_path(&mut self, clip_path: &ClipPath) {
        let mut c = self.collecting.get();
        c.clipped = true;
        self.collecting.set(c);
        self.inner.push_clip_path(clip_path);
    }

    fn push_transparency_group(&mut self, opacity: f32, mask: Option<SoftMask<'a>>, blend_mode: BlendMode) {
        self.unbounded();
        self.inner.push_transparency_group(opacity, mask, blend_mode);
    }

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        paint: &Paint<'a>,
        draw_mode: &GlyphDrawMode,
    ) {
        match (glyph, draw_mode) {
            (_, GlyphDrawMode::Invisible | GlyphDrawMode::Clip) => {}
            (Glyph::Outline(o), mode) => {
                let key = o.identifier().cache_key();
                let outline = *self.glyph_bounds.entry(key).or_insert_with(|| {
                    let path = o.outline();
                    (!path.elements().is_empty()).then(|| path.bounding_box())
                });
                if let Some(outline) = outline {
                    let path = outline.to_path(0.1);
                    let mode = match mode {
                        GlyphDrawMode::Stroke(s) => PathDrawMode::Stroke(s.clone()),
                        _ => PathDrawMode::Fill(crate::FillRule::NonZero),
                    };
                    self.add_path(&path, transform * glyph_transform, &mode);
                }
            }
            (Glyph::Type3(_), _) => self.unbounded(),
        }
        self.inner.draw_glyph(glyph, transform, glyph_transform, paint, draw_mode);
    }

    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        self.unbounded();
        self.inner.draw_image(image, transform);
    }

    fn pop_clip_path(&mut self) {
        self.inner.pop_clip_path();
    }

    fn pop_transparency_group(&mut self) {
        self.inner.pop_transparency_group();
    }

    fn draw_rect(&mut self, rect: &Rect, transform: Affine, paint: &Paint<'a>, draw_mode: &PathDrawMode) {
        self.add_path(&rect.to_path(0.1), transform, draw_mode);
        self.inner.draw_rect(rect, transform, paint, draw_mode);
    }

    fn begin_marked_content(&mut self, tag: &[u8], mcid: Option<i32>) {
        self.inner.begin_marked_content(tag, mcid);
    }

    fn actual_text(&mut self, text: &[u8]) {
        self.inner.actual_text(text);
    }

    fn end_marked_content(&mut self) {
        self.inner.end_marked_content();
    }

    fn begin_annotations(&mut self) {
        self.inner.begin_annotations();
    }

    fn end_annotations(&mut self) {
        self.inner.end_annotations();
    }

    fn set_knockout(&mut self) {
        self.unbounded();
        self.inner.set_knockout();
    }

    fn knockout_shape(&mut self, shape: &BezPath) {
        self.unbounded();
        self.inner.knockout_shape(shape);
    }

    fn in_knockout_group(&mut self) -> bool {
        self.inner.in_knockout_group()
    }

    fn knockout_image(&mut self, image: &crate::RasterImage<'_>, transform: Affine) {
        self.unbounded();
        self.inner.knockout_image(image, transform);
    }

    fn knockout_erase_wanted(&mut self) -> bool {
        self.inner.knockout_erase_wanted()
    }

    fn begin_knockout_erase(&mut self) {
        self.unbounded();
        self.inner.begin_knockout_erase();
    }

    fn end_knockout_erase(&mut self) {
        self.inner.end_knockout_erase();
    }
}
