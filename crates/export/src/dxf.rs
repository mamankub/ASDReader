//! Export a PDF ▸ AutoCAD DXF: one page's vector content ([`asdreader_render::vector`]) as a DXF
//! drawing (AutoCAD 2013 format, UTF-8) that AutoCAD, GstarCAD, BricsCAD and other CAD programs
//! open as editable model-space geometry.
//!
//! - Lines stay LINEs and connected lines LWPOLYLINEs. Circles and arcs come back as CIRCLE and
//!   ARC entities (or arc segments of a polyline): both when the PDF draws them as Bézier curves
//!   and when the program that made it cut them into short straight lines, as plot drivers do.
//!   Other curves are exact SPLINEs.
//! - Filled areas are SOLID hatches. A white page background is left out.
//! - Text is TEXT in a text style of the PDF's own font (family, bold, italic), sized as CAD
//!   programs size TrueType text (by the font's ascent), unless it is exported as outlines.
//! - Images are IMAGE entities referring to files the caller writes next to the drawing.
//! - Colours are true colours (black is colour 7, black on paper and white on a dark screen),
//!   line widths are lineweights (paper sizes, whatever the drawing scale), dash patterns are
//!   linetypes, and PDF layers are layers; content outside any layer is put on `PDF_Geometry`,
//!   `PDF_Solid Fills`, `PDF_Text` and `PDF_Images`.
//! - Coordinates are the page's, from its lower-left corner, scaled from points to drawing units
//!   by [`DxfOptions::scale`] (for a 1:100 sheet in millimetres, a 3 m wall measures 3000).

use std::collections::HashMap;
use std::fmt::Write as _;

use asdreader_render::vector::{ImageItem, PathItem, PathPaint, Pt, Seg, Subpath, TextItem, VectorItem, VectorPage};

/// The drawing's units ($INSUNITS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Millimetres,
    Centimetres,
    Metres,
    Inches,
}

impl Units {
    /// Millimetres in one unit.
    pub fn millimetres(self) -> f64 {
        match self {
            Units::Millimetres => 1.0,
            Units::Centimetres => 10.0,
            Units::Metres => 1000.0,
            Units::Inches => 25.4,
        }
    }

    fn insunits(self) -> i32 {
        match self {
            Units::Inches => 1,
            Units::Millimetres => 4,
            Units::Centimetres => 5,
            Units::Metres => 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DxfOptions {
    pub units: Units,
    /// The drawing's scale: 100 for a sheet drawn at 1:100, so lengths come out at full size.
    pub scale: f64,
    /// The file each of the page's images ([`VectorPage::images`]) is written to, relative to
    /// the drawing (`None`: that image is left out).
    pub image_files: Vec<Option<String>>,
}

impl DxfOptions {
    /// Drawing units in one PDF point.
    pub fn units_per_point(&self) -> f64 {
        let scale = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        25.4 / 72.0 * scale / self.units.millimetres()
    }
}

/// What a DXF export made, for the message that reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DxfSummary {
    pub lines: usize,
    pub polylines: usize,
    pub circles: usize,
    pub arcs: usize,
    pub splines: usize,
    pub hatches: usize,
    pub texts: usize,
    pub images: usize,
}

/// Lineweights a DXF entity may have, in hundredths of a millimetre.
const LINEWEIGHTS: [i32; 24] = [0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200, 211];

/// How far a vertex may be from the circle it is taken to lie on, in points: plot drivers round
/// coordinates to a grid about this fine.
const ARC_TOLERANCE: f64 = 0.08;
/// The fewest straight pieces a run must have to be taken for an arc.
const ARC_MIN_PIECES: usize = 4;
/// The largest turn one straight piece of an arc may make, in degrees.
const ARC_MAX_STEP: f64 = 40.0;

type P = [f64; 2];

fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}

fn dist(a: P, b: P) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn cross(a: P, b: P) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

/// The circle through three points.
fn circle3(a: P, b: P, c: P) -> Option<(P, f64)> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, b2, c2) = (a[0] * a[0] + a[1] * a[1], b[0] * b[0] + b[1] * b[1], c[0] * c[0] + c[1] * c[1]);
    let x = (a2 * (b[1] - c[1]) + b2 * (c[1] - a[1]) + c2 * (a[1] - b[1])) / d;
    let y = (a2 * (c[0] - b[0]) + b2 * (a[0] - c[0]) + c2 * (b[0] - a[0])) / d;
    let center = [x, y];
    let r = dist(center, a);
    (x.is_finite() && y.is_finite() && r.is_finite()).then_some((center, r))
}

/// The least-squares circle through points (Kåsa's fit).
fn fit_circle(points: &[P]) -> Option<(P, f64)> {
    let n = points.len() as f64;
    if points.len() < 3 {
        return None;
    }
    let (mx, my) = (points.iter().map(|p| p[0]).sum::<f64>() / n, points.iter().map(|p| p[1]).sum::<f64>() / n);
    let (mut suu, mut svv, mut suv, mut suuu, mut svvv, mut suvv, mut svuu) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for p in points {
        let (u, v) = (p[0] - mx, p[1] - my);
        suu += u * u;
        svv += v * v;
        suv += u * v;
        suuu += u * u * u;
        svvv += v * v * v;
        suvv += u * v * v;
        svuu += v * u * u;
    }
    let det = suu * svv - suv * suv;
    if det.abs() < 1e-18 {
        return None;
    }
    let (b1, b2) = (0.5 * (suuu + suvv), 0.5 * (svvv + svuu));
    let uc = (b1 * svv - b2 * suv) / det;
    let vc = (suu * b2 - suv * b1) / det;
    let r = (uc * uc + vc * vc + (suu + svv) / n).sqrt();
    let center = [uc + mx, vc + my];
    (center[0].is_finite() && center[1].is_finite() && r.is_finite() && r > 0.0).then_some((center, r))
}

/// The signed angle (radians, counterclockwise positive) from `a` to `b` seen from `c`.
fn turn(c: P, a: P, b: P) -> f64 {
    let (u, v) = (sub(a, c), sub(b, c));
    cross(u, v).atan2(u[0] * v[0] + u[1] * v[1])
}

/// One piece of a polyline: to `to`, straight or along an arc of signed sweep `sweep` (radians,
/// counterclockwise positive) about `center`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Piece {
    Line { to: P },
    Arc { to: P, center: P, radius: f64, sweep: f64 },
}

impl Piece {
    fn to(&self) -> P {
        match *self {
            Piece::Line { to } | Piece::Arc { to, .. } => to,
        }
    }

    /// The polyline bulge of the piece (the tangent of a quarter of its sweep).
    fn bulge(&self) -> f64 {
        match *self {
            Piece::Line { .. } => 0.0,
            Piece::Arc { sweep, .. } => (sweep / 4.0).tan(),
        }
    }
}

/// The signed turn (radians) at the vertex between pieces `u` and `v`.
fn bend(u: P, v: P) -> f64 {
    cross(u, v).atan2(u[0] * v[0] + u[1] * v[1])
}

/// The arc through `points` if they lie on one circle within `tol` (least squares): its centre,
/// radius and signed sweep from the first point to the last.
fn arc_fit(points: &[P], tol: f64) -> Option<(P, f64, f64)> {
    let closed = points.len() > 3 && points.first() == points.last();
    let fit = if closed { points.get(..points.len() - 1)? } else { points };
    let (center, r) = fit_circle(fit)?;
    let tol = tol + r * 0.002;
    if points.iter().any(|p| (dist(*p, center) - r).abs() > tol) {
        return None;
    }
    let sweep: f64 = points.windows(2).map(|w| turn(center, w[0], w[1])).sum();
    (sweep.abs() <= std::f64::consts::TAU + 1e-6 && sweep.abs() > 1e-9).then_some((center, r, sweep))
}

/// Whether the turn `t` at a vertex between pieces of lengths `a` and `b` continues an arc
/// that turns `mean` per piece over pieces `chord` long: the same way, by about as much, with
/// pieces of about the same length. Rounding to the plot grid moves a vertex by up to `tol`,
/// which turns a piece by about `tol` over its length.
fn continues(t: f64, mean: f64, a: f64, b: f64, chord: f64, tol: f64) -> bool {
    let noise = 2.5 * tol / a.min(b).max(1e-12);
    let len_noise = 2.5 * tol;
    t.signum() == mean.signum()
        && (t - mean).abs() <= 0.4 * mean.abs() + noise
        && (a - chord).abs() <= 0.4 * chord + len_noise
        && (b - chord).abs() <= 0.4 * chord + len_noise
}

/// Whether a vertex turning by `t` can be inside an arc a CAD program cut into straight pieces.
fn smooth(t: f64, a: f64, b: f64, tol: f64) -> bool {
    let noise = 2.5 * tol / a.min(b).max(1e-12);
    t.abs() <= ARC_MAX_STEP.to_radians() + noise && t.abs() >= 0.3f64.to_radians() && a > 0.0 && b > 0.0
}

/// The straight pieces from `points[0]` through `points[1..]`, with runs of them that lie on an
/// arc (turning one way by nearly equal steps, on a circle within `tol`) replaced by that arc. A
/// polyline that comes back to its start and is one arc all round is one whole-circle piece.
fn fit_arcs(points: &[P], tol: f64) -> Vec<Piece> {
    let n = points.len().saturating_sub(1);
    let piece = |k: usize| -> P { sub(points.get(k + 1).copied().unwrap_or_default(), points.get(k).copied().unwrap_or_default()) };
    let len = |k: usize| -> f64 {
        let d = piece(k);
        d[0].hypot(d[1])
    };
    // The turn at vertex k, between piece k - 1 and piece k.
    let bend_at = |k: usize| -> f64 { bend(piece(k - 1), piece(k)) };
    let lines = |from: usize, to: usize| (from..to).map(|k| Piece::Line { to: points.get(k + 1).copied().unwrap_or_default() });

    // A whole circle: every vertex, the start included, continues one arc.
    if n > ARC_MIN_PIECES && points.first() == points.last() {
        let turns: Vec<f64> = (0..n).map(|k| bend(piece((k + n - 1) % n), piece(k))).collect();
        let mean = turns.iter().sum::<f64>() / n as f64;
        let chord = (0..n).map(len).sum::<f64>() / n as f64;
        let all = (0..n).all(|k| {
            let (a, b) = (len((k + n - 1) % n), len(k));
            smooth(turns[k], a, b, tol) && continues(turns[k], mean, a, b, chord, tol)
        });
        if all
            && let Some((center, radius, sweep)) = arc_fit(points, tol)
            && (sweep.abs() - std::f64::consts::TAU).abs() < 1e-3
        {
            return vec![Piece::Arc { to: points.first().copied().unwrap_or_default(), center, radius, sweep }];
        }
    }

    let mut out = Vec::new();
    // Pieces before `done` are written; the candidate arc runs from piece `start`.
    let mut done = 0;
    let mut start = 0;
    let (mut mean, mut chord, mut count) = (0.0, 0.0, 0usize);
    let close = |out: &mut Vec<Piece>, done: &mut usize, start: usize, end: usize| {
        // Pieces start..end (end exclusive) as one arc, when there are enough and they fit.
        if end >= start + ARC_MIN_PIECES
            && let Some((center, radius, sweep)) = points.get(start..=end).and_then(|run| arc_fit(run, tol))
        {
            out.extend(lines(*done, start));
            out.push(Piece::Arc { to: points.get(end).copied().unwrap_or_default(), center, radius, sweep });
            *done = end;
            return true;
        }
        false
    };
    let mut v = 1;
    while v < n {
        let t = bend_at(v);
        let (a, b) = (len(v - 1), len(v));
        let fits = smooth(t, a, b, tol) && (count == 0 || continues(t, mean / count as f64, a, b, chord / (count + 1) as f64, tol));
        if fits {
            if count == 0 {
                start = v - 1;
                chord = a;
            }
            mean += t;
            chord += b;
            count += 1;
        } else {
            if count > 0 {
                close(&mut out, &mut done, start, v);
            }
            count = 0;
            mean = 0.0;
            chord = 0.0;
            // The vertex that broke the run may start the next one.
            if smooth(t, a, b, tol) && v > done {
                start = v - 1;
                chord = a + b;
                mean = t;
                count = 1;
            }
        }
        v += 1;
    }
    if count > 0 {
        close(&mut out, &mut done, start, n);
    }
    out.extend(lines(done, n));
    out
}

/// Whether the cubic `p0 c1 c2 p3` is (within `tol`) an arc of a circle, as PDF writers draw
/// arcs: the arc's centre, radius and signed sweep if so.
fn cubic_arc(p0: P, c1: P, c2: P, p3: P, tol: f64) -> Option<(P, f64, f64)> {
    let at = |t: f64| {
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        [a * p0[0] + b * c1[0] + c * c2[0] + d * p3[0], a * p0[1] + b * c1[1] + c * c2[1] + d * p3[1]]
    };
    let (center, r) = circle3(p0, at(0.5), p3)?;
    let tol = tol + r * 0.001;
    if (1..10).any(|k| (dist(at(f64::from(k) / 10.0), center) - r).abs() > tol) {
        return None;
    }
    // The curve's start tangent must be the circle's (not a curve that wanders onto it).
    let tangent = sub(c1, p0);
    let radial = sub(p0, center);
    if tangent[0].hypot(tangent[1]) > 1e-12 && (radial[0] * tangent[0] + radial[1] * tangent[1]).abs() > 0.05 * r * tangent[0].hypot(tangent[1]) {
        return None;
    }
    let sweep = {
        let half = turn(center, p0, at(0.5));
        let rest = turn(center, at(0.5), p3);
        half + rest
    };
    (sweep.abs() > 1e-6 && sweep.abs() <= std::f64::consts::PI * 1.01).then_some((center, r, sweep))
}

/// A path piece in drawing units: polyline pieces, or a run of cubic curves that aren't arcs.
enum Run {
    Poly { start: P, pieces: Vec<Piece> },
    Curves { start: P, curves: Vec<(P, P, P)> },
}

/// A subpath's geometry in drawing units, arcs recognised.
fn runs(s: &Subpath, map: &impl Fn(Pt) -> P, tol: f64) -> (Vec<Run>, bool) {
    let mut out: Vec<Run> = Vec::new();
    let start = map(s.start);
    let mut cursor = start;
    // Straight vertices waiting for arc fitting, from `cursor` on.
    let mut pending: Vec<P> = vec![start];
    let mut pieces: Vec<Piece> = Vec::new();
    let mut poly_start = start;
    let mut segs: Vec<Seg> = s.segs.clone();
    let closes = s.closed && s.segs.last().map(|g| map(g.end())) != Some(start);
    if closes {
        segs.push(Seg::Line(s.start));
    }
    let flush_lines = |pending: &mut Vec<P>, pieces: &mut Vec<Piece>| {
        if pending.len() > 1 {
            pieces.extend(fit_arcs(pending, tol));
        }
        let last = pending.last().copied();
        pending.clear();
        if let Some(l) = last {
            pending.push(l);
        }
    };
    for g in &segs {
        match *g {
            Seg::Line(p) => {
                let p = map(p);
                if p != cursor {
                    pending.push(p);
                    cursor = p;
                }
            }
            Seg::Cubic(a, b, p) => {
                let (a, b, p) = (map(a), map(b), map(p));
                match cubic_arc(cursor, a, b, p, tol) {
                    Some((center, radius, sweep)) => {
                        flush_lines(&mut pending, &mut pieces);
                        pieces.push(Piece::Arc { to: p, center, radius, sweep });
                        pending = vec![p];
                    }
                    None => {
                        flush_lines(&mut pending, &mut pieces);
                        if !pieces.is_empty() {
                            out.push(Run::Poly { start: poly_start, pieces: std::mem::take(&mut pieces) });
                        }
                        match out.last_mut() {
                            Some(Run::Curves { curves, .. }) if curves.last().map(|c| c.2) == Some(cursor) => curves.push((a, b, p)),
                            _ => out.push(Run::Curves { start: cursor, curves: vec![(a, b, p)] }),
                        }
                        poly_start = p;
                        pending = vec![p];
                    }
                }
                cursor = p;
            }
        }
    }
    flush_lines(&mut pending, &mut pieces);
    if !pieces.is_empty() {
        out.push(Run::Poly { start: poly_start, pieces });
    }
    // One polyline that comes back to its start is a closed one.
    let closed = matches!(out.as_slice(), [Run::Poly { start, pieces }] if pieces.last().map(|p| p.to()) == Some(*start));
    (out, closed)
}

/// Consecutive arc pieces of one circle as one (so a circle drawn as four Bézier quarters is one
/// circle).
fn merge_arcs(pieces: Vec<Piece>, tol: f64) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for p in pieces {
        if let (Some(Piece::Arc { to, center, radius, sweep }), Piece::Arc { to: to2, center: c2, radius: r2, sweep: s2 }) = (out.last_mut(), p)
            && dist(*center, c2) <= tol + 0.002 * *radius
            && (*radius - r2).abs() <= tol + 0.002 * *radius
            && sweep.signum() == s2.signum()
            && (*sweep + s2).abs() <= std::f64::consts::TAU + 1e-6
        {
            *to = to2;
            *sweep += s2;
            continue;
        }
        out.push(p);
    }
    out
}

/// sRGB → the nearest AutoCAD Color Index.
fn aci(rgb: [u8; 3]) -> i32 {
    let target = rgb.map(f64::from);
    let mut best = (7, f64::INFINITY);
    for (i, c) in aci_palette().iter().enumerate().skip(1) {
        let d = (0..3).map(|k| (c[k] - target[k]).powi(2)).sum::<f64>();
        if d < best.1 {
            best = (i as i32, d);
        }
    }
    best.0
}

/// The AutoCAD Color Index palette: 1–9 fixed colours, 10–249 24 hues in ten shades, 250–255
/// grays.
fn aci_palette() -> &'static [[f64; 3]; 256] {
    static PALETTE: std::sync::OnceLock<[[f64; 3]; 256]> = std::sync::OnceLock::new();
    PALETTE.get_or_init(|| {
        let mut p = [[0.0; 3]; 256];
        let fixed = [
            [0.0, 0.0, 0.0],
            [255.0, 0.0, 0.0],
            [255.0, 255.0, 0.0],
            [0.0, 255.0, 0.0],
            [0.0, 255.0, 255.0],
            [0.0, 0.0, 255.0],
            [255.0, 0.0, 255.0],
            [255.0, 255.0, 255.0],
            [128.0, 128.0, 128.0],
            [192.0, 192.0, 192.0],
        ];
        p[..10].copy_from_slice(&fixed);
        let shades = [(1.0, 1.0), (1.0, 0.5), (0.8, 1.0), (0.8, 0.5), (0.6, 1.0), (0.6, 0.5), (0.5, 1.0), (0.5, 0.5), (0.3, 1.0), (0.3, 0.5)];
        for hue in 0..24 {
            let h = hue as f64 * 15.0;
            for (k, (v, s)) in shades.iter().enumerate() {
                let c = v * s;
                let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
                let (r, g, b) = match (h / 60.0) as i32 {
                    0 => (c, x, 0.0),
                    1 => (x, c, 0.0),
                    2 => (0.0, c, x),
                    3 => (0.0, x, c),
                    4 => (x, 0.0, c),
                    _ => (c, 0.0, x),
                };
                let m = v - c;
                p[10 + hue * 10 + k] = [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0];
            }
        }
        for (k, g) in [51.0, 91.0, 132.0, 173.0, 214.0, 255.0].iter().enumerate() {
            p[250 + k] = [*g; 3];
        }
        p
    })
}

/// A stroke width in points → the nearest lineweight (hundredths of a millimetre).
fn lineweight(width: f64) -> i32 {
    let mm100 = width * 25.4 / 72.0 * 100.0;
    let mut best = 0;
    for w in LINEWEIGHTS {
        if (f64::from(w) - mm100).abs() < (f64::from(best) - mm100).abs() {
            best = w;
        }
    }
    best
}

/// A name usable for a layer, text style or linetype: no characters DXF forbids in names.
fn symbol_name(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if "<>/\\\":;?*|,=`".contains(c) || c.is_control() { '_' } else { c }).take(250).collect::<String>().trim().to_string();
    if cleaned.is_empty() { "_".into() } else { cleaned }
}

/// The file a font family is usually installed as on Windows, for the text style's font file
/// (CAD programs find TrueType fonts by the family name the style also carries).
fn font_file(family: &str) -> String {
    let known = [
        ("TH SarabunPSK", "THSarabun.ttf"),
        ("TH Sarabun New", "THSarabunNew.ttf"),
        ("Cordia New", "cordia.ttf"),
        ("CordiaUPC", "cordiau.ttf"),
        ("Angsana New", "angsa.ttf"),
        ("AngsanaUPC", "angsau.ttf"),
        ("Browallia New", "browa.ttf"),
        ("BrowalliaUPC", "browau.ttf"),
        ("EucrosiaUPC", "eucrosiau.ttf"),
        ("FreesiaUPC", "frees.ttf"),
        ("IrisUPC", "irisu.ttf"),
        ("JasmineUPC", "jasmineu.ttf"),
        ("KodchiangUPC", "kodchiangu.ttf"),
        ("LilyUPC", "lilyu.ttf"),
        ("DilleniaUPC", "dilleniau.ttf"),
        ("Leelawadee UI", "LeelawUI.ttf"),
        ("Leelawadee", "leelawad.ttf"),
        ("Tahoma", "tahoma.ttf"),
        ("Arial", "arial.ttf"),
        ("Arial Black", "ariblk.ttf"),
        ("Arial Narrow", "arialn.ttf"),
        ("Times New Roman", "times.ttf"),
        ("Courier New", "cour.ttf"),
        ("Calibri", "calibri.ttf"),
        ("Cambria", "cambria.ttc"),
        ("Segoe UI", "segoeui.ttf"),
        ("Microsoft Sans Serif", "micross.ttf"),
        ("Verdana", "verdana.ttf"),
        ("Georgia", "georgia.ttf"),
        ("Century Gothic", "GOTHIC.TTF"),
        ("Isocpeur", "isocpeur.ttf"),
    ];
    known
        .iter()
        .find(|(f, _)| f.eq_ignore_ascii_case(family))
        .map(|(_, file)| file.to_string())
        .unwrap_or_else(|| format!("{}.ttf", family.chars().filter(|c| !c.is_whitespace()).collect::<String>()))
}

/// A number as DXF writes it: no exponent, no more digits than it has.
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0.0".into();
    }
    let r = if v.abs() < 1e9 { (v * 1e9).round() / 1e9 } else { v };
    let r = if r == 0.0 { 0.0 } else { r };
    let s = format!("{r}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

/// A DXF text value: one line, and `%%` (AutoCAD's control-code prefix) kept as written.
fn text_value(s: &str) -> String {
    let s: String = s.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    if s.contains("%%") { s.replace('%', "%%%") } else { s }
}

struct Out {
    buf: String,
    next: u64,
}

impl Out {
    fn new() -> Self {
        Self { buf: String::new(), next: 0x30 }
    }

    fn handle(&mut self) -> u64 {
        let h = self.next;
        self.next += 1;
        h
    }

    fn g(&mut self, code: i32, value: impl std::fmt::Display) {
        let _ = write!(self.buf, "{code:>3}\r\n{value}\r\n");
    }

    fn f(&mut self, code: i32, v: f64) {
        let s = num(v);
        self.g(code, s);
    }

    fn h(&mut self, code: i32, handle: u64) {
        self.g(code, format!("{handle:X}"));
    }

    fn xy(&mut self, code: i32, p: P) {
        self.f(code, p[0]);
        self.f(code + 10, p[1]);
    }

    fn xyz(&mut self, code: i32, p: P) {
        self.xy(code, p);
        self.f(code + 20, 0.0);
    }
}

const MODEL_SPACE: u64 = 0x17;
const MODEL_BLOCK: u64 = 0x18;
const MODEL_END: u64 = 0x19;
const MODEL_LAYOUT: u64 = 0x1A;
const PAPER_SPACE: u64 = 0x1B;
const PAPER_BLOCK: u64 = 0x1C;
const PAPER_END: u64 = 0x1D;
const PAPER_LAYOUT: u64 = 0x1E;
const ROOT: u64 = 0xA;
const GROUPS: u64 = 0xC;
const LAYOUTS: u64 = 0xD;
const PLOT_STYLES: u64 = 0x12;
const PLOT_STYLE_NORMAL: u64 = 0x13;

/// How an entity looks: layer, colour, lineweight and linetype.
struct Look<'a> {
    layer: &'a str,
    color: [u8; 3],
    lineweight: Option<i32>,
    linetype: Option<&'a str>,
}

fn entity(o: &mut Out, kind: &str, look: &Look<'_>) -> u64 {
    let h = o.handle();
    o.g(0, kind);
    o.h(5, h);
    o.h(330, MODEL_SPACE);
    o.g(100, "AcDbEntity");
    o.g(8, look.layer);
    if let Some(lt) = look.linetype {
        o.g(6, lt);
    }
    let [r, g, b] = look.color;
    if r.max(g).max(b) <= 8 {
        o.g(62, 7);
    } else {
        o.g(62, aci(look.color));
    }
    if let Some(lw) = look.lineweight {
        o.g(370, lw);
    }
    if r.max(g).max(b) > 8 {
        o.g(420, (i32::from(r) << 16) | (i32::from(g) << 8) | i32::from(b));
    }
    h
}

/// The drawing's layers, linetypes and text styles, gathered from the content.
struct Tables {
    layers: Vec<String>,
    layer_keys: HashMap<String, usize>,
    linetypes: Vec<(String, Vec<f64>)>,
    linetype_keys: HashMap<Vec<i64>, usize>,
    styles: Vec<(String, String, bool, bool)>,
    style_keys: HashMap<usize, usize>,
}

impl Tables {
    fn layer(&mut self, name: &str) -> String {
        let name = symbol_name(name);
        // Layer names are compared without case; a second name differing only in case gets a suffix.
        let key = name.to_lowercase();
        if let Some(&i) = self.layer_keys.get(&key) {
            return self.layers.get(i).cloned().unwrap_or(name);
        }
        self.layer_keys.insert(key, self.layers.len());
        self.layers.push(name.clone());
        name
    }

    fn linetype(&mut self, dash: &[f64], units: f64) -> Option<String> {
        if dash.is_empty() {
            return None;
        }
        // An odd-length array repeats to make whole on/off pairs (ISO 32000-2 §8.4.3.6).
        let pattern: Vec<f64> =
            if dash.len() % 2 == 1 { dash.iter().chain(dash.iter()).map(|d| d * units).collect() } else { dash.iter().map(|d| d * units).collect() };
        let key: Vec<i64> = pattern.iter().map(|d| (d * 1e4).round() as i64).collect();
        if let Some(&i) = self.linetype_keys.get(&key) {
            return self.linetypes.get(i).map(|(n, _)| n.clone());
        }
        let name = format!("PDF_DASH{}", self.linetypes.len() + 1);
        self.linetype_keys.insert(key, self.linetypes.len());
        self.linetypes.push((name.clone(), pattern));
        Some(name)
    }

    fn style(&mut self, page: &VectorPage, font: usize) -> String {
        if let Some(&i) = self.style_keys.get(&font) {
            return self.styles.get(i).map(|s| s.0.clone()).unwrap_or_else(|| "Standard".into());
        }
        let Some(face) = page.fonts.get(font) else { return "Standard".into() };
        let mut name = symbol_name(&face.family);
        if face.bold {
            name.push_str(" Bold");
        }
        if face.italic {
            name.push_str(" Italic");
        }
        let taken = |n: &str| self.styles.iter().any(|s| s.0.eq_ignore_ascii_case(n)) || n.eq_ignore_ascii_case("Standard");
        let base = name.clone();
        let mut k = 2;
        while taken(&name) {
            name = format!("{base} {k}");
            k += 1;
        }
        self.style_keys.insert(font, self.styles.len());
        self.styles.push((name.clone(), face.family.clone(), face.bold, face.italic));
        name
    }
}

struct Placed {
    entity: u64,
    reactor: u64,
    image: usize,
}

/// Page `page` as a DXF drawing (the file's text, UTF-8).
pub fn dxf(page: &VectorPage, options: &DxfOptions) -> (String, DxfSummary) {
    let s = options.units_per_point();
    let height = page.height;
    let map = move |p: Pt| -> P { [p[0] * s, (height - p[1]) * s] };
    let tol = ARC_TOLERANCE * s;
    let mut tables = Tables {
        layers: Vec::new(),
        layer_keys: HashMap::new(),
        linetypes: Vec::new(),
        linetype_keys: HashMap::new(),
        styles: Vec::new(),
        style_keys: HashMap::new(),
    };
    tables.layer("0");
    let mut summary = DxfSummary::default();
    let mut o = Out::new();
    let mut placed: Vec<Placed> = Vec::new();
    let page_area = page.width * page.height;
    // Each image file's definition, made when an image is first placed.
    let mut defs: HashMap<usize, u64> = HashMap::new();
    for item in &page.items {
        match item {
            VectorItem::Path(p) => {
                let default = if matches!(p.paint, PathPaint::Fill { .. }) { "PDF_Solid Fills" } else { "PDF_Geometry" };
                let layer = tables.layer(p.layer.and_then(|l| page.layers.get(l)).map_or(default, |s| s.as_str()));
                if is_background(p, page_area) {
                    continue;
                }
                path_entities(&mut o, &mut tables, p, &layer, &map, s, tol, &mut summary);
            }
            VectorItem::Text(t) => {
                let layer = tables.layer(t.layer.and_then(|l| page.layers.get(l)).map_or("PDF_Text", |s| s.as_str()));
                let style = tables.style(page, t.font);
                text_entity(&mut o, page, t, &layer, &style, &map, s);
                summary.texts += 1;
            }
            VectorItem::Image(im) => {
                let Some(Some(_)) = options.image_files.get(im.image) else { continue };
                let layer = tables.layer(im.layer.and_then(|l| page.layers.get(l)).map_or("PDF_Images", |s| s.as_str()));
                if let Some(p) = image_entity(&mut o, page, im, &layer, &map, s, &mut defs) {
                    placed.push(p);
                    summary.images += 1;
                }
            }
        }
    }
    let entities = std::mem::take(&mut o.buf);
    let (image_dict, image_vars) = if placed.is_empty() { (None, None) } else { (Some(o.handle()), Some(o.handle())) };
    let mut tables_handles = TableHandles::default();
    let tables_text = write_tables(&mut o, &tables, &mut tables_handles, page, s);
    let (w, h) = (page.width * s, page.height * s);

    let mut doc = Out { buf: String::new(), next: o.next };
    write_header(&mut doc, options, [w, h], tables_handles.seed.max(o.next));
    write_classes(&mut doc, !placed.is_empty());
    doc.buf.push_str(&tables_text);
    write_blocks(&mut doc);
    doc.g(0, "SECTION");
    doc.g(2, "ENTITIES");
    doc.buf.push_str(&entities);
    doc.g(0, "ENDSEC");
    write_objects(&mut doc, page, options, &placed, &defs, image_dict, image_vars, [w, h]);
    doc.g(0, "EOF");
    (doc.buf, summary)
}

/// A white fill over most of the page: the paper, not drawing content.
fn is_background(p: &PathItem, page_area: f64) -> bool {
    let PathPaint::Fill { .. } = p.paint else { return false };
    if p.color.iter().any(|c| *c < 250) {
        return false;
    }
    let mut area = 0.0;
    for s in &p.subpaths {
        let mut prev = s.start;
        let mut a = 0.0;
        for g in &s.segs {
            let e = g.end();
            a += prev[0] * e[1] - e[0] * prev[1];
            prev = e;
        }
        a += prev[0] * s.start[1] - s.start[0] * prev[1];
        area += a.abs() / 2.0;
    }
    page_area > 0.0 && area >= 0.9 * page_area
}

#[allow(clippy::too_many_arguments)]
fn path_entities(o: &mut Out, tables: &mut Tables, p: &PathItem, layer: &str, map: &impl Fn(Pt) -> P, s: f64, tol: f64, summary: &mut DxfSummary) {
    match &p.paint {
        PathPaint::Fill { .. } => {
            let mut loops: Vec<(Vec<P>, Vec<f64>)> = Vec::new();
            for sp in &p.subpaths {
                let (runs, _) = runs(sp, map, tol);
                let mut verts: Vec<P> = Vec::new();
                let mut bulges: Vec<f64> = Vec::new();
                for run in runs {
                    match run {
                        Run::Poly { start, pieces } => {
                            if verts.last() != Some(&start) {
                                verts.push(start);
                                bulges.push(0.0);
                            }
                            for piece in merge_arcs(pieces, tol) {
                                // Arcs of a closed boundary are kept under a half turn each.
                                for piece in split_arc(piece, verts.last().copied().unwrap_or(start)) {
                                    if let Some(b) = bulges.last_mut() {
                                        *b = piece.bulge();
                                    }
                                    verts.push(piece.to());
                                    bulges.push(0.0);
                                }
                            }
                        }
                        Run::Curves { start, curves } => {
                            if verts.last() != Some(&start) {
                                verts.push(start);
                                bulges.push(0.0);
                            }
                            let mut from = start;
                            for (a, b, e) in curves {
                                for q in flatten_cubic(from, a, b, e, (0.02 * s).max(1e-9)) {
                                    verts.push(q);
                                    bulges.push(0.0);
                                }
                                from = e;
                            }
                        }
                    }
                }
                // The boundary closes itself: a last vertex on the first is dropped.
                if verts.len() > 1 && verts.first() == verts.last() {
                    verts.pop();
                    bulges.pop();
                }
                if verts.len() >= 3 {
                    loops.push((verts, bulges));
                }
            }
            if loops.is_empty() {
                return;
            }
            let look = Look { layer, color: p.color, lineweight: None, linetype: None };
            entity(o, "HATCH", &look);
            o.g(100, "AcDbHatch");
            o.xyz(10, [0.0, 0.0]);
            o.f(210, 0.0);
            o.f(220, 0.0);
            o.f(230, 1.0);
            o.g(2, "SOLID");
            o.g(70, 1);
            o.g(71, 0);
            o.g(91, loops.len());
            for (verts, bulges) in &loops {
                let has_bulge = bulges.iter().any(|b| *b != 0.0);
                o.g(92, 2);
                o.g(72, i32::from(has_bulge));
                o.g(73, 1);
                o.g(93, verts.len());
                for (v, b) in verts.iter().zip(bulges) {
                    o.xy(10, *v);
                    if has_bulge {
                        o.f(42, *b);
                    }
                }
                o.g(97, 0);
            }
            o.g(75, 0);
            o.g(76, 1);
            o.g(98, 0);
            summary.hatches += 1;
        }
        PathPaint::Stroke(stroke) => {
            let lw = lineweight(stroke.width);
            // Wider than the widest lineweight: a polyline of that width instead.
            let wide = stroke.width * 25.4 / 72.0 > 2.2;
            let linetype = tables.linetype(&stroke.dash, s);
            let look = Look { layer, color: p.color, lineweight: Some(if wide { 0 } else { lw }), linetype: linetype.as_deref() };
            let width = if wide { stroke.width * s } else { 0.0 };
            for sp in &p.subpaths {
                let (runs, closed) = runs(sp, map, tol);
                for run in runs {
                    match run {
                        Run::Poly { start, pieces } => poly_entity(o, &look, start, merge_arcs(pieces, tol), closed, width, summary),
                        Run::Curves { start, curves } => {
                            spline_entity(o, &look, start, &curves);
                            summary.splines += 1;
                        }
                    }
                }
            }
        }
    }
}

/// An arc piece over a half turn as two (a hatch boundary's bulges stay moderate).
fn split_arc(piece: Piece, from: P) -> Vec<Piece> {
    match piece {
        Piece::Arc { to, center, radius, sweep } if sweep.abs() > std::f64::consts::PI => {
            let a = (from[1] - center[1]).atan2(from[0] - center[0]) + sweep / 2.0;
            let mid = [center[0] + radius * a.cos(), center[1] + radius * a.sin()];
            vec![Piece::Arc { to: mid, center, radius, sweep: sweep / 2.0 }, Piece::Arc { to, center, radius, sweep: sweep / 2.0 }]
        }
        p => vec![p],
    }
}

/// A cubic as points within `tol` of it (the start left out).
fn flatten_cubic(p0: P, c1: P, c2: P, p3: P, tol: f64) -> Vec<P> {
    let len = dist(p0, c1) + dist(c1, c2) + dist(c2, p3);
    let n = ((len / tol).sqrt().ceil() as usize).clamp(1, 256);
    (1..=n)
        .map(|k| {
            let t = k as f64 / n as f64;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            [a * p0[0] + b * c1[0] + c * c2[0] + d * p3[0], a * p0[1] + b * c1[1] + c * c2[1] + d * p3[1]]
        })
        .collect()
}

fn poly_entity(o: &mut Out, look: &Look<'_>, start: P, pieces: Vec<Piece>, closed: bool, width: f64, summary: &mut DxfSummary) {
    // A whole circle.
    if let [Piece::Arc { center, radius, sweep, .. }] = pieces.as_slice()
        && (sweep.abs() - std::f64::consts::TAU).abs() < 1e-3
        && width == 0.0
    {
        entity(o, "CIRCLE", look);
        o.g(100, "AcDbCircle");
        o.xyz(10, *center);
        o.f(40, *radius);
        summary.circles += 1;
        return;
    }
    match pieces.as_slice() {
        [] => {}
        [Piece::Line { to }] if width == 0.0 => {
            entity(o, "LINE", look);
            o.g(100, "AcDbLine");
            o.xyz(10, start);
            o.xyz(11, *to);
            summary.lines += 1;
        }
        [Piece::Arc { center, radius, sweep, .. }] if width == 0.0 && sweep.abs() < std::f64::consts::TAU - 1e-3 => {
            // ARC runs counterclockwise from its start angle to its end angle.
            let a0 = (start[1] - center[1]).atan2(start[0] - center[0]).to_degrees();
            let a1 = a0 + sweep.to_degrees();
            let (from, to) = if *sweep > 0.0 { (a0, a1) } else { (a1, a0) };
            entity(o, "ARC", look);
            o.g(100, "AcDbCircle");
            o.xyz(10, *center);
            o.f(40, *radius);
            o.g(100, "AcDbArc");
            o.f(50, from.rem_euclid(360.0));
            o.f(51, to.rem_euclid(360.0));
            summary.arcs += 1;
        }
        _ => {
            let mut verts = vec![start];
            let mut bulges = vec![0.0];
            for piece in pieces {
                for piece in split_arc(piece, verts.last().copied().unwrap_or(start)) {
                    if let Some(b) = bulges.last_mut() {
                        *b = piece.bulge();
                    }
                    verts.push(piece.to());
                    bulges.push(0.0);
                }
            }
            if closed && verts.len() > 2 && verts.first() == verts.last() {
                verts.pop();
                bulges.pop();
            }
            entity(o, "LWPOLYLINE", look);
            o.g(100, "AcDbPolyline");
            o.g(90, verts.len());
            o.g(70, (if closed { 1 } else { 0 }) | 128);
            if width > 0.0 {
                o.f(43, width);
            }
            for (v, b) in verts.iter().zip(&bulges) {
                o.xy(10, *v);
                if *b != 0.0 {
                    o.f(42, *b);
                }
            }
            summary.polylines += 1;
        }
    }
}

/// Cubic Bézier pieces as one exact degree-3 SPLINE.
fn spline_entity(o: &mut Out, look: &Look<'_>, start: P, curves: &[(P, P, P)]) {
    let n = curves.len();
    entity(o, "SPLINE", look);
    o.g(100, "AcDbSpline");
    o.f(210, 0.0);
    o.f(220, 0.0);
    o.f(230, 1.0);
    o.g(70, 8);
    o.g(71, 3);
    o.g(72, 3 * n + 5);
    o.g(73, 3 * n + 1);
    o.g(74, 0);
    o.f(42, 1e-10);
    o.f(43, 1e-10);
    let mut knots = vec![0.0; 4];
    for k in 1..n {
        knots.extend([k as f64; 3]);
    }
    knots.extend([n as f64; 4]);
    for k in knots {
        o.f(40, k);
    }
    o.xyz(10, start);
    for (a, b, e) in curves {
        o.xyz(10, *a);
        o.xyz(10, *b);
        o.xyz(10, *e);
    }
}

fn text_entity(o: &mut Out, page: &VectorPage, t: &TextItem, layer: &str, style: &str, map: &impl Fn(Pt) -> P, s: f64) {
    let ascent = page.fonts.get(t.font).map_or(0.9, |f| f.ascent);
    let look = Look { layer, color: t.color, lineweight: None, linetype: None };
    entity(o, "TEXT", &look);
    o.g(100, "AcDbText");
    o.xyz(10, map(t.origin));
    o.f(40, t.size * ascent * s);
    o.g(1, text_value(&t.text));
    // The page is y down; the drawing y up.
    let rotation = (-t.direction[1]).atan2(t.direction[0]).to_degrees();
    if rotation.abs() > 1e-9 {
        o.f(50, rotation.rem_euclid(360.0));
    }
    if (t.width_factor - 1.0).abs() > 1e-6 {
        o.f(41, t.width_factor);
    }
    if t.oblique.abs() > 1e-6 {
        o.f(51, t.oblique.rem_euclid(360.0));
    }
    o.g(7, style);
    o.g(100, "AcDbText");
}

fn image_entity(
    o: &mut Out,
    page: &VectorPage,
    im: &ImageItem,
    layer: &str,
    map: &impl Fn(Pt) -> P,
    s: f64,
    defs: &mut HashMap<usize, u64>,
) -> Option<Placed> {
    let img = page.images.get(im.image)?;
    let (w, h) = (f64::from(img.width), f64::from(img.height));
    let [a, b, c, d, e, f] = im.transform;
    let at = |x: f64, y: f64| -> Pt { [a * x + c * y + e, b * x + d * y + f] };
    // Pixel space is y down from the top; the image's own v axis points up its rows.
    let origin = map(at(0.0, h));
    let u = [a * s, -b * s];
    let v = [-c * s, d * s];
    if cross(u, v).abs().is_nan() || cross(u, v).abs() <= 1e-18 {
        return None;
    }
    let look = Look { layer, color: [0, 0, 0], lineweight: None, linetype: None };
    let def = *defs.entry(im.image).or_insert_with(|| o.handle());
    let handle = entity(o, "IMAGE", &look);
    let reactor = o.handle();
    o.g(100, "AcDbRasterImage");
    o.g(90, 0);
    o.xyz(10, origin);
    o.xyz(11, u);
    o.xyz(12, v);
    o.f(13, w);
    o.f(23, h);
    o.h(340, def);
    o.g(70, 7);
    o.g(280, i32::from(im.clip.is_some()));
    o.g(281, 50);
    o.g(282, 50);
    o.g(283, 0);
    o.h(360, reactor);
    // The clip boundary is in pixel coordinates from the centre of the top-left pixel.
    let boundary: Vec<P> = match im.clip {
        Some([x0, y0, x1, y1]) => {
            let det = a * d - b * c;
            if det.abs() < 1e-18 {
                return None;
            }
            let inv = |p: Pt| -> P {
                let (x, y) = (p[0] - e, p[1] - f);
                [(d * x - c * y) / det - 0.5, (-b * x + a * y) / det - 0.5]
            };
            [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(inv).to_vec()
        }
        None => vec![[-0.5, -0.5], [w - 0.5, h - 0.5]],
    };
    let rect = boundary.len() == 2;
    o.g(71, if rect { 1 } else { 2 });
    o.g(91, if rect { 2 } else { boundary.len() + 1 });
    for p in &boundary {
        o.xy(14, *p);
    }
    if !rect && let Some(first) = boundary.first() {
        o.xy(14, *first);
    }
    o.g(290, 0);
    Some(Placed { entity: handle, reactor, image: im.image })
}

#[derive(Default)]
struct TableHandles {
    seed: u64,
}

fn table(o: &mut Out, name: &str, handle: u64, count: usize) {
    o.g(0, "TABLE");
    o.g(2, name);
    o.h(5, handle);
    o.h(330, 0);
    o.g(100, "AcDbSymbolTable");
    o.g(70, count);
}

fn record(o: &mut Out, kind: &str, owner: u64, subclass: &str) -> u64 {
    let h = o.handle();
    o.g(0, kind);
    o.h(5, h);
    o.h(330, owner);
    o.g(100, "AcDbSymbolTableRecord");
    o.g(100, subclass);
    h
}

fn write_tables(o: &mut Out, t: &Tables, handles: &mut TableHandles, page: &VectorPage, s: f64) -> String {
    let saved = std::mem::take(&mut o.buf);
    o.g(0, "SECTION");
    o.g(2, "TABLES");
    // The view opens on the whole sheet.
    let (w, h) = (page.width * s, page.height * s);
    table(o, "VPORT", 0x8, 1);
    record(o, "VPORT", 0x8, "AcDbViewportTableRecord");
    o.g(2, "*Active");
    o.g(70, 0);
    o.xy(10, [0.0, 0.0]);
    o.xy(11, [1.0, 1.0]);
    o.xy(12, [w / 2.0, h / 2.0]);
    o.xy(13, [0.0, 0.0]);
    o.xy(14, [10.0 * s, 10.0 * s]);
    o.xy(15, [10.0 * s, 10.0 * s]);
    // The view direction is (0, 0, 1). xyz() would also write group 36 = 0,
    // leaving a duplicate Z and a zero-length direction in readers that take
    // the first triplet. Write X/Y once, then its Z once.
    o.xy(16, [0.0, 0.0]);
    o.f(36, 1.0);
    o.xyz(17, [0.0, 0.0]);
    o.f(40, (h * 1.05).max(1e-6));
    o.f(41, if h > 0.0 { w / h } else { 1.0 });
    o.f(42, 50.0);
    o.f(43, 0.0);
    o.f(44, 0.0);
    o.f(50, 0.0);
    o.f(51, 0.0);
    for (code, v) in [(71, 0), (72, 1000), (73, 1), (74, 3), (75, 0), (76, 0), (77, 0), (78, 0), (281, 0), (65, 1)] {
        o.g(code, v);
    }
    o.g(0, "ENDTAB");

    table(o, "LTYPE", 0x2, 3 + t.linetypes.len());
    for name in ["ByBlock", "ByLayer", "Continuous"] {
        record(o, "LTYPE", 0x2, "AcDbLinetypeTableRecord");
        o.g(2, name);
        o.g(70, 0);
        o.g(3, if name == "Continuous" { "Solid line" } else { "" });
        o.g(72, 65);
        o.g(73, 0);
        o.f(40, 0.0);
    }
    for (name, pattern) in &t.linetypes {
        record(o, "LTYPE", 0x2, "AcDbLinetypeTableRecord");
        o.g(2, name);
        o.g(70, 0);
        o.g(3, "PDF dash pattern");
        o.g(72, 65);
        o.g(73, pattern.len());
        o.f(40, pattern.iter().sum::<f64>());
        for (k, d) in pattern.iter().enumerate() {
            o.f(49, if k % 2 == 0 { *d } else { -*d });
            o.g(74, 0);
        }
    }
    o.g(0, "ENDTAB");

    table(o, "LAYER", 0x1, t.layers.len());
    for name in &t.layers {
        record(o, "LAYER", 0x1, "AcDbLayerTableRecord");
        o.g(2, name);
        o.g(70, 0);
        o.g(62, 7);
        o.g(6, "Continuous");
        o.g(370, -3);
        o.h(390, PLOT_STYLE_NORMAL);
    }
    o.g(0, "ENDTAB");

    table(o, "STYLE", 0x5, 1 + t.styles.len());
    record(o, "STYLE", 0x5, "AcDbTextStyleTableRecord");
    o.g(2, "Standard");
    o.g(70, 0);
    o.f(40, 0.0);
    o.f(41, 1.0);
    o.f(50, 0.0);
    o.g(71, 0);
    o.f(42, 2.5);
    o.g(3, "arial.ttf");
    o.g(4, "");
    o.g(1001, "ACAD");
    o.g(1000, "Arial");
    o.g(1071, 34);
    for (name, family, bold, italic) in &t.styles {
        record(o, "STYLE", 0x5, "AcDbTextStyleTableRecord");
        o.g(2, name);
        o.g(70, 0);
        o.f(40, 0.0);
        o.f(41, 1.0);
        o.f(50, 0.0);
        o.g(71, 0);
        o.f(42, 2.5);
        o.g(3, font_file(family));
        o.g(4, "");
        // The TrueType family and its bold and italic flags, as AutoCAD stores them.
        o.g(1001, "ACAD");
        o.g(1000, family);
        o.g(1071, 34 | if *bold { 0x0200_0000 } else { 0 } | if *italic { 0x0100_0000 } else { 0 });
    }
    o.g(0, "ENDTAB");

    table(o, "VIEW", 0x7, 0);
    o.g(0, "ENDTAB");
    table(o, "UCS", 0x6, 0);
    o.g(0, "ENDTAB");
    table(o, "APPID", 0x3, 1);
    record(o, "APPID", 0x3, "AcDbRegAppTableRecord");
    o.g(2, "ACAD");
    o.g(70, 0);
    o.g(0, "ENDTAB");

    table(o, "DIMSTYLE", 0x4, 1);
    o.g(100, "AcDbDimStyleTable");
    let h = o.handle();
    o.g(0, "DIMSTYLE");
    o.h(105, h);
    o.h(330, 0x4);
    o.g(100, "AcDbSymbolTableRecord");
    o.g(100, "AcDbDimStyleTableRecord");
    o.g(2, "Standard");
    o.g(70, 0);
    o.g(0, "ENDTAB");

    table(o, "BLOCK_RECORD", 0x9, 2);
    for (handle, name, layout) in [(MODEL_SPACE, "*Model_Space", MODEL_LAYOUT), (PAPER_SPACE, "*Paper_Space", PAPER_LAYOUT)] {
        o.g(0, "BLOCK_RECORD");
        o.h(5, handle);
        o.h(330, 0x9);
        o.g(100, "AcDbSymbolTableRecord");
        o.g(100, "AcDbBlockTableRecord");
        o.g(2, name);
        o.h(340, layout);
        o.g(70, 0);
        o.g(280, 1);
        o.g(281, 0);
    }
    o.g(0, "ENDTAB");
    o.g(0, "ENDSEC");
    handles.seed = o.next;
    std::mem::replace(&mut o.buf, saved)
}

fn write_header(o: &mut Out, options: &DxfOptions, size: P, seed: u64) {
    o.g(0, "SECTION");
    o.g(2, "HEADER");
    let var = |o: &mut Out, name: &str| o.g(9, name);
    var(o, "$ACADVER");
    o.g(1, "AC1027");
    var(o, "$ACADMAINTVER");
    o.g(70, 105);
    var(o, "$DWGCODEPAGE");
    o.g(3, "ANSI_1252");
    var(o, "$INSBASE");
    o.xyz(10, [0.0, 0.0]);
    var(o, "$EXTMIN");
    o.xyz(10, [0.0, 0.0]);
    var(o, "$EXTMAX");
    o.xyz(10, size);
    var(o, "$LIMMIN");
    o.xy(10, [0.0, 0.0]);
    var(o, "$LIMMAX");
    o.xy(10, size);
    var(o, "$LTSCALE");
    o.f(40, 1.0);
    var(o, "$TEXTSTYLE");
    o.g(7, "Standard");
    var(o, "$CLAYER");
    o.g(8, "0");
    var(o, "$CELTYPE");
    o.g(6, "ByLayer");
    var(o, "$CECOLOR");
    o.g(62, 256);
    var(o, "$LUNITS");
    o.g(70, 2);
    var(o, "$LUPREC");
    o.g(70, 4);
    var(o, "$INSUNITS");
    o.g(70, options.units.insunits());
    var(o, "$MEASUREMENT");
    o.g(70, i32::from(options.units != Units::Inches));
    var(o, "$LWDISPLAY");
    o.g(290, 1);
    var(o, "$PSLTSCALE");
    o.g(70, 0);
    var(o, "$HANDSEED");
    o.h(5, seed.saturating_add(0x100));
    o.g(0, "ENDSEC");
}

fn write_classes(o: &mut Out, images: bool) {
    o.g(0, "SECTION");
    o.g(2, "CLASSES");
    let classes: &[(&str, &str, i32, i32)] =
        &[("ACDBDICTIONARYWDFLT", "AcDbDictionaryWithDefault", 0, 0), ("ACDBPLACEHOLDER", "AcDbPlaceHolder", 0, 0), ("LAYOUT", "AcDbLayout", 0, 0)];
    let image_classes: &[(&str, &str, i32, i32)] = &[
        ("RASTERVARIABLES", "AcDbRasterVariables", 0, 0),
        ("IMAGE", "AcDbRasterImage", 2175, 1),
        ("IMAGEDEF", "AcDbRasterImageDef", 0, 0),
        ("IMAGEDEF_REACTOR", "AcDbRasterImageDefReactor", 1, 0),
    ];
    for (name, class, flags, is_entity) in classes.iter().chain(if images { image_classes.iter() } else { [].iter() }) {
        o.g(0, "CLASS");
        o.g(1, name);
        o.g(2, class);
        o.g(3, if name.contains("IMAGE") || name.starts_with("RASTER") { "ISM" } else { "ObjectDBX Classes" });
        o.g(90, flags);
        o.g(91, 0);
        o.g(280, 0);
        o.g(281, is_entity);
    }
    o.g(0, "ENDSEC");
}

fn write_blocks(o: &mut Out) {
    o.g(0, "SECTION");
    o.g(2, "BLOCKS");
    for (block, end, owner, name) in [(MODEL_BLOCK, MODEL_END, MODEL_SPACE, "*Model_Space"), (PAPER_BLOCK, PAPER_END, PAPER_SPACE, "*Paper_Space")] {
        o.g(0, "BLOCK");
        o.h(5, block);
        o.h(330, owner);
        o.g(100, "AcDbEntity");
        if owner == PAPER_SPACE {
            o.g(67, 1);
        }
        o.g(8, "0");
        o.g(100, "AcDbBlockBegin");
        o.g(2, name);
        o.g(70, 0);
        o.xyz(10, [0.0, 0.0]);
        o.g(3, name);
        o.g(1, "");
        o.g(0, "ENDBLK");
        o.h(5, end);
        o.h(330, owner);
        o.g(100, "AcDbEntity");
        if owner == PAPER_SPACE {
            o.g(67, 1);
        }
        o.g(8, "0");
        o.g(100, "AcDbBlockEnd");
    }
    o.g(0, "ENDSEC");
}

fn dictionary(o: &mut Out, handle: u64, owner: u64, entries: &[(&str, u64)]) {
    o.g(0, "DICTIONARY");
    o.h(5, handle);
    o.h(330, owner);
    o.g(100, "AcDbDictionary");
    o.g(281, 1);
    for (name, h) in entries {
        o.g(3, name);
        o.h(350, *h);
    }
}

fn layout(o: &mut Out, handle: u64, name: &str, order: i32, block: u64, size: P) {
    o.g(0, "LAYOUT");
    o.h(5, handle);
    o.h(330, LAYOUTS);
    o.g(100, "AcDbPlotSettings");
    o.g(1, "");
    o.g(4, "");
    o.g(6, "");
    for code in [40, 41, 42, 43] {
        o.f(code, 0.0);
    }
    o.f(44, 0.0);
    o.f(45, 0.0);
    for code in [46, 47, 48, 49, 140, 141] {
        o.f(code, 0.0);
    }
    o.f(142, 1.0);
    o.f(143, 1.0);
    o.g(70, if order == 0 { 1024 } else { 0 });
    o.g(72, 1);
    o.g(73, 0);
    o.g(74, 5);
    o.g(7, "");
    o.g(75, 16);
    o.g(76, 0);
    o.g(77, 2);
    o.g(78, 300);
    o.f(147, 1.0);
    o.f(148, 0.0);
    o.f(149, 0.0);
    o.g(100, "AcDbLayout");
    o.g(1, name);
    o.g(70, 1);
    o.g(71, order);
    o.xy(10, [0.0, 0.0]);
    o.xy(11, size);
    o.xyz(12, [0.0, 0.0]);
    o.xyz(14, [0.0, 0.0]);
    o.xyz(15, size);
    o.f(146, 0.0);
    o.xyz(13, [0.0, 0.0]);
    o.xyz(16, [1.0, 0.0]);
    o.xyz(17, [0.0, 1.0]);
    o.g(76, 1);
    o.h(330, block);
}

#[allow(clippy::too_many_arguments)]
fn write_objects(
    o: &mut Out,
    page: &VectorPage,
    options: &DxfOptions,
    placed: &[Placed],
    defs: &HashMap<usize, u64>,
    image_dict: Option<u64>,
    image_vars: Option<u64>,
    size: P,
) {
    o.g(0, "SECTION");
    o.g(2, "OBJECTS");
    let mut root: Vec<(&str, u64)> = vec![("ACAD_GROUP", GROUPS), ("ACAD_LAYOUT", LAYOUTS), ("ACAD_PLOTSTYLENAME", PLOT_STYLES)];
    if let (Some(d), Some(v)) = (image_dict, image_vars) {
        root.push(("ACAD_IMAGE_DICT", d));
        root.push(("ACAD_IMAGE_VARS", v));
    }
    dictionary(o, ROOT, 0, &root);
    dictionary(o, GROUPS, ROOT, &[]);
    dictionary(o, LAYOUTS, ROOT, &[("Layout1", PAPER_LAYOUT), ("Model", MODEL_LAYOUT)]);
    o.g(0, "ACDBDICTIONARYWDFLT");
    o.h(5, PLOT_STYLES);
    o.h(330, ROOT);
    o.g(100, "AcDbDictionary");
    o.g(281, 1);
    o.g(3, "Normal");
    o.h(350, PLOT_STYLE_NORMAL);
    o.g(100, "AcDbDictionaryWithDefault");
    o.h(340, PLOT_STYLE_NORMAL);
    o.g(0, "ACDBPLACEHOLDER");
    o.h(5, PLOT_STYLE_NORMAL);
    o.h(330, PLOT_STYLES);
    layout(o, MODEL_LAYOUT, "Model", 0, MODEL_SPACE, size);
    layout(o, PAPER_LAYOUT, "Layout1", 1, PAPER_SPACE, size);
    if let (Some(dict), Some(vars)) = (image_dict, image_vars) {
        let mut names: Vec<(usize, u64, String)> =
            defs.iter().filter_map(|(image, h)| options.image_files.get(*image).cloned().flatten().map(|f| (*image, *h, f))).collect();
        names.sort();
        let entries: Vec<(String, u64)> = names
            .iter()
            .map(|(_, h, f)| (symbol_name(std::path::Path::new(f).file_stem().and_then(|s| s.to_str()).unwrap_or(f.as_str())), *h))
            .collect();
        let refs: Vec<(&str, u64)> = entries.iter().map(|(n, h)| (n.as_str(), *h)).collect();
        dictionary(o, dict, ROOT, &refs);
        o.g(0, "RASTERVARIABLES");
        o.h(5, vars);
        o.h(330, ROOT);
        o.g(100, "AcDbRasterVariables");
        o.g(90, 0);
        o.g(70, 0);
        o.g(71, 1);
        o.g(
            72,
            match options.units {
                Units::Millimetres => 1,
                Units::Centimetres => 2,
                Units::Metres => 3,
                Units::Inches => 5,
            },
        );
        for (image, h, file) in &names {
            let Some(img) = page.images.get(*image) else { continue };
            o.g(0, "IMAGEDEF");
            o.h(5, *h);
            o.g(102, "{ACAD_REACTORS");
            o.h(330, dict);
            for p in placed.iter().filter(|p| p.image == *image) {
                o.h(330, p.reactor);
            }
            o.g(102, "}");
            o.h(330, dict);
            o.g(100, "AcDbRasterImageDef");
            o.g(90, 0);
            o.g(1, file);
            o.f(10, f64::from(img.width));
            o.f(20, f64::from(img.height));
            o.f(11, 1.0);
            o.f(21, 1.0);
            o.g(280, 1);
            o.g(281, 0);
        }
        for p in placed {
            o.g(0, "IMAGEDEF_REACTOR");
            o.h(5, p.reactor);
            o.h(330, p.entity);
            o.g(100, "AcDbRasterImageDefReactor");
            o.g(90, 2);
            o.h(330, p.entity);
        }
    }
    o.g(0, "ENDSEC");
}

/// The drawing scale a page states in a title block or under a view ("SCALE 1:100",
/// "มาตราส่วน 1:50"): the most common one, `None` when it states none.
pub fn stated_scale(page: &VectorPage) -> Option<f64> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    // A ratio read as the label's is not counted again on its own.
    let mut counted = std::collections::HashSet::new();
    let texts: Vec<&str> = page
        .items
        .iter()
        .filter_map(|i| if let VectorItem::Text(t) = i { Some(t.text.as_str()) } else { None })
        .chain(page.outlined_text.iter().map(String::as_str))
        .collect();
    for (k, text) in texts.iter().enumerate() {
        // The ratio may be in the same run as the label or the run after it.
        let lower = text.to_lowercase();
        let labelled = lower.contains("scale") || text.contains("มาตราส่วน");
        let candidates: &[usize] = if labelled { &[k, k + 1] } else { &[k] };
        for &i in candidates {
            let Some(c) = texts.get(i) else { continue };
            if counted.contains(&i) {
                continue;
            }
            if let Some(n) = ratio(c)
                && (labelled || c.trim_start().starts_with("1:") || c.trim_start().starts_with("1 :"))
            {
                *counts.entry(n).or_default() += 1;
                counted.insert(i);
                // One ratio per label.
                break;
            }
        }
    }
    counts.into_iter().max_by_key(|(n, c)| (*c, -*n)).map(|(n, _)| n as f64)
}

/// The `N` of a "1:N" in `text`.
fn ratio(text: &str) -> Option<i64> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let at = compact.find("1:")?;
    let digits: String = compact.get(at + 2..)?.chars().take_while(|c| c.is_ascii_digit() || *c == ',').filter(|c| *c != ',').collect();
    let n: i64 = digits.parse().ok()?;
    (1..=100_000).contains(&n).then_some(n)
}

#[cfg(test)]
mod tests;
