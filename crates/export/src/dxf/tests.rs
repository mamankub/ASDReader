use super::*;
use asdreader_render::vector::{FontFace, ImageItem, Stroke, VectorImage};

/// The (code, value) pairs of a DXF file.
fn pairs(dxf: &str) -> Vec<(i32, String)> {
    let lines: Vec<&str> = dxf.split("\r\n").collect();
    assert_eq!(lines.last(), Some(&""), "the file ends with a line break");
    lines
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[code, value]| (code.trim().parse::<i32>().unwrap_or_else(|_| panic!("group code {code:?}")), value.to_string()))
        .collect()
}

/// Each entity or object of a section: its type and its groups.
fn records<'a>(pairs: &'a [(i32, String)], section: &str) -> Vec<(&'a str, Vec<(i32, &'a str)>)> {
    let start = pairs.iter().position(|(c, v)| *c == 2 && v == section).unwrap_or_else(|| panic!("no {section} section"));
    let mut out: Vec<(&str, Vec<(i32, &str)>)> = Vec::new();
    for (c, v) in &pairs[start + 1..] {
        if *c == 0 {
            if v == "ENDSEC" {
                break;
            }
            out.push((v.as_str(), Vec::new()));
        } else if let Some(last) = out.last_mut() {
            last.1.push((*c, v.as_str()));
        }
    }
    out
}

fn entities(dxf: &str) -> Vec<(String, Vec<(i32, String)>)> {
    let p = pairs(dxf);
    records(&p, "ENTITIES").into_iter().map(|(k, g)| (k.to_string(), g.into_iter().map(|(c, v)| (c, v.to_string())).collect())).collect()
}

fn get(groups: &[(i32, String)], code: i32) -> Vec<&str> {
    groups.iter().filter(|(c, _)| *c == code).map(|(_, v)| v.as_str()).collect()
}

fn f(v: &str) -> f64 {
    v.parse().unwrap_or_else(|_| panic!("number {v:?}"))
}

fn page(items: Vec<VectorItem>) -> VectorPage {
    VectorPage { width: 200.0, height: 100.0, items, ..Default::default() }
}

fn stroke(points: &[Pt], closed: bool) -> VectorItem {
    VectorItem::Path(PathItem {
        subpaths: vec![Subpath { start: points[0], segs: points[1..].iter().map(|p| Seg::Line(*p)).collect(), closed }],
        paint: PathPaint::Stroke(Stroke { width: 0.72, dash: Vec::new(), dash_offset: 0.0 }),
        color: [0, 0, 0],
        layer: None,
    })
}

fn opts(units: Units, scale: f64) -> DxfOptions {
    DxfOptions { units, scale, image_files: Vec::new() }
}

/// A circle as a plot driver writes one: `n` straight pieces, every vertex rounded to `grid`.
fn polygon_circle(center: Pt, r: f64, n: usize, grid: f64) -> Vec<Pt> {
    (0..=n)
        .map(|k| {
            let a = std::f64::consts::TAU * k as f64 / n as f64;
            [((center[0] + r * a.cos()) / grid).round() * grid, ((center[1] + r * a.sin()) / grid).round() * grid]
        })
        .collect()
}

#[test]
fn every_handle_is_unique_and_every_reference_points_at_one() {
    let mut items = vec![stroke(&[[10.0, 10.0], [50.0, 10.0]], false), stroke(&polygon_circle([100.0, 50.0], 8.0, 28, 0.12), false)];
    items.push(VectorItem::Image(ImageItem {
        image: 0,
        transform: [10.0, 0.0, 0.0, 10.0, 20.0, 20.0],
        clip: Some([25.0, 25.0, 40.0, 40.0]),
        layer: None,
    }));
    items.push(VectorItem::Image(ImageItem { image: 0, transform: [10.0, 0.0, 0.0, 10.0, 60.0, 20.0], clip: None, layer: None }));
    let mut p = page(items);
    p.images.push(VectorImage { width: 2, height: 2, rgba: vec![0; 16], jpeg: None });
    let o = DxfOptions { image_files: vec![Some("plan_page_1_image_1.png".into())], ..opts(Units::Millimetres, 1.0) };
    let (dxf, summary) = dxf(&p, &o);
    assert_eq!((summary.lines, summary.circles, summary.images), (1, 1, 2));
    assert!(!dxf.contains("{IMAGEDEF:"), "every image points at its definition");
    let p = pairs(&dxf);
    let mut handles = std::collections::HashSet::new();
    // The header's $HANDSEED is a group 5 too; the handles come after it.
    let body = p.iter().position(|(c, v)| *c == 2 && v == "CLASSES").unwrap();
    for (c, v) in &p[body..] {
        if *c == 5 || *c == 105 {
            assert!(handles.insert(v.clone()), "handle {v} is used twice");
        }
    }
    for (c, v) in &p {
        if matches!(*c, 330 | 340 | 350 | 360 | 390) && v != "0" {
            assert!(handles.contains(v), "group {c} points at {v}, which nothing has");
        }
    }
    let seed = p.iter().skip_while(|(c, v)| !(*c == 9 && v == "$HANDSEED")).nth(1).map(|(_, v)| u64::from_str_radix(v, 16).unwrap()).unwrap();
    assert!(handles.iter().all(|h| u64::from_str_radix(h, 16).unwrap() < seed), "$HANDSEED is past every handle");
    // The image's reactor names the image, and the definition names both reactors.
    let objects = records(&p, "OBJECTS");
    let def = objects.iter().find(|(k, _)| *k == "IMAGEDEF").expect("an image definition");
    assert_eq!(def.1.iter().filter(|(c, _)| *c == 330).count(), 4, "the image dictionary, two reactors, the owner");
    assert!(def.1.contains(&(1, "plan_page_1_image_1.png")));
    assert_eq!(objects.iter().filter(|(k, _)| *k == "IMAGEDEF_REACTOR").count(), 2);
    // The clipped image has a polygon boundary in pixel space (x from 0.5 to 2, centre-based).
    let images: Vec<_> = entities(&dxf).into_iter().filter(|(k, _)| k == "IMAGE").collect();
    assert_eq!(get(&images[0].1, 280), ["1"]);
    assert_eq!(get(&images[0].1, 71), ["2"]);
    let xs: Vec<f64> = get(&images[0].1, 14).into_iter().map(f).collect();
    assert_eq!(xs, [0.0, 1.5, 1.5, 0.0, 0.0]);
    assert_eq!(get(&images[1].1, 71), ["1"]);
}

#[test]
fn a_circle_cut_into_rounded_straight_pieces_is_a_circle() {
    let (dxf, s) = dxf(&page(vec![stroke(&polygon_circle([100.0, 50.0], 8.0, 28, 0.12), false)]), &opts(Units::Millimetres, 1.0));
    assert_eq!((s.circles, s.polylines, s.lines, s.arcs), (1, 0, 0, 0), "{s:?}");
    let e = entities(&dxf);
    let r = f(get(&e[0].1, 40)[0]);
    let mm = 25.4 / 72.0;
    assert!((r - 8.0 * mm).abs() < 0.02 * mm, "radius {r}");
    assert!((f(get(&e[0].1, 10)[0]) - 100.0 * mm).abs() < 0.05 * mm);
    assert!((f(get(&e[0].1, 20)[0]) - 50.0 * mm).abs() < 0.05 * mm);
}

#[test]
fn polygons_and_corners_stay_straight() {
    let octagon = polygon_circle([50.0, 50.0], 20.0, 8, 1e-9);
    let square = [[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0], [10.0, 10.0]];
    // A zigzag turns by equal steps but each way in turn.
    let zigzag: Vec<Pt> = (0..12).map(|k| [k as f64 * 5.0, if k % 2 == 0 { 0.0 } else { 1.0 }]).collect();
    let (_, s) = dxf(&page(vec![stroke(&octagon, false), stroke(&square, false), stroke(&zigzag, false)]), &opts(Units::Millimetres, 1.0));
    assert_eq!((s.circles, s.arcs, s.polylines), (0, 0, 3), "{s:?}");
}

#[test]
fn a_rounded_rectangle_is_one_closed_polyline_with_four_quarter_arcs() {
    // Corners of radius 5 cut into 6 pieces each.
    let mut pts = Vec::new();
    let corner = |pts: &mut Vec<Pt>, c: Pt, from: f64| {
        for k in 0..=6 {
            let a = (from + 90.0 * k as f64 / 6.0).to_radians();
            pts.push([c[0] + 5.0 * a.cos(), c[1] + 5.0 * a.sin()]);
        }
    };
    corner(&mut pts, [45.0, 45.0], 0.0);
    corner(&mut pts, [15.0, 45.0], 90.0);
    corner(&mut pts, [15.0, 15.0], 180.0);
    corner(&mut pts, [45.0, 15.0], 270.0);
    pts.push(pts[0]);
    let (dxf, s) = dxf(&page(vec![stroke(&pts, true)]), &opts(Units::Millimetres, 1.0));
    assert_eq!((s.polylines, s.circles, s.arcs, s.lines), (1, 0, 0, 0), "{s:?}");
    let e = entities(&dxf);
    assert_eq!(get(&e[0].1, 70), ["129"], "closed (1) with continuous linetype generation (128)");
    let bulges: Vec<f64> = get(&e[0].1, 42).into_iter().map(f).collect();
    // On the page (y down) the corners turn clockwise; in the drawing (y up) counterclockwise.
    let quarter = (std::f64::consts::PI / 8.0).tan();
    assert_eq!(bulges.len(), 4, "{bulges:?}");
    assert!(bulges.iter().all(|b| (b.abs() - quarter).abs() < 1e-3), "{bulges:?}");
    assert_eq!(get(&e[0].1, 90), ["8"]);
}

#[test]
fn bezier_circles_and_arcs_are_circles_and_arcs_and_other_curves_splines() {
    let k = 0.5522847498 * 10.0;
    let c = [100.0, 50.0];
    let circle = Subpath {
        start: [c[0] + 10.0, c[1]],
        segs: vec![
            Seg::Cubic([c[0] + 10.0, c[1] + k], [c[0] + k, c[1] + 10.0], [c[0], c[1] + 10.0]),
            Seg::Cubic([c[0] - k, c[1] + 10.0], [c[0] - 10.0, c[1] + k], [c[0] - 10.0, c[1]]),
            Seg::Cubic([c[0] - 10.0, c[1] - k], [c[0] - k, c[1] - 10.0], [c[0], c[1] - 10.0]),
            Seg::Cubic([c[0] + k, c[1] - 10.0], [c[0] + 10.0, c[1] - k], [c[0] + 10.0, c[1]]),
        ],
        closed: true,
    };
    let quarter = Subpath { start: [30.0, 20.0], segs: vec![Seg::Cubic([30.0, 20.0 + k], [20.0 + k, 30.0], [20.0, 30.0])], closed: false };
    let wave = Subpath {
        start: [0.0, 0.0],
        segs: vec![Seg::Cubic([10.0, 30.0], [20.0, -30.0], [30.0, 0.0]), Seg::Cubic([40.0, 30.0], [50.0, -30.0], [60.0, 0.0])],
        closed: false,
    };
    let item = |s: Subpath| {
        VectorItem::Path(PathItem {
            subpaths: vec![s],
            paint: PathPaint::Stroke(Stroke { width: 0.0, dash: vec![], dash_offset: 0.0 }),
            color: [0, 0, 0],
            layer: None,
        })
    };
    let (dxf, s) = dxf(&page(vec![item(circle), item(quarter), item(wave)]), &opts(Units::Millimetres, 72.0 / 25.4));
    assert_eq!((s.circles, s.arcs, s.splines, s.polylines, s.lines), (1, 1, 1, 0, 0), "{s:?}");
    let e = entities(&dxf);
    let arc = e.iter().find(|(k, _)| k == "ARC").unwrap();
    // The page is y down: from (30, 20) to (20, 30) about (20, 20) is, in the drawing (y up
    // from 100), from 0° clockwise to 270°, so the ARC runs counterclockwise from 270° to 360°.
    assert!((f(get(&arc.1, 50)[0]) - 270.0).abs() < 0.01, "{:?}", arc.1);
    assert!(f(get(&arc.1, 51)[0]).abs() < 0.01 || (f(get(&arc.1, 51)[0]) - 360.0).abs() < 0.01, "{:?}", arc.1);
    let spline = e.iter().find(|(k, _)| k == "SPLINE").unwrap();
    assert_eq!(get(&spline.1, 72), ["11"]);
    assert_eq!(get(&spline.1, 73), ["7"]);
    assert_eq!(get(&spline.1, 40).into_iter().map(f).collect::<Vec<_>>(), [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]);
}

#[test]
fn colours_lineweights_and_dashes() {
    let mut red = stroke(&[[0.0, 0.0], [10.0, 0.0]], false);
    if let VectorItem::Path(p) = &mut red {
        p.color = [255, 0, 0];
        p.paint = PathPaint::Stroke(Stroke { width: 2.0, dash: vec![3.0], dash_offset: 0.0 });
    }
    let (dxf, _) = dxf(&page(vec![stroke(&[[0.0, 5.0], [10.0, 5.0]], false), red]), &opts(Units::Millimetres, 1.0));
    let e = entities(&dxf);
    // Black is colour 7 (black on paper, white on a dark screen) with no true colour.
    assert_eq!((get(&e[0].1, 62), get(&e[0].1, 420), get(&e[0].1, 370)), (vec!["7"], vec![], vec!["25"]));
    assert_eq!((get(&e[1].1, 62), get(&e[1].1, 420), get(&e[1].1, 370)), (vec!["1"], vec!["16711680"], vec!["70"]));
    assert_eq!(get(&e[1].1, 6), ["PDF_DASH1"]);
    // [3] is 3 on, 3 off, in drawing units.
    let p = pairs(&dxf);
    let ltype = records(&p, "TABLES").into_iter().find(|(k, g)| *k == "LTYPE" && g.contains(&(2, "PDF_DASH1"))).unwrap();
    let mm = 3.0 * 25.4 / 72.0;
    let lengths: Vec<f64> = ltype.1.iter().filter(|(c, _)| *c == 49).map(|(_, v)| f(v)).collect();
    assert!(lengths.len() == 2 && (lengths[0] - mm).abs() < 1e-6 && (lengths[1] + mm).abs() < 1e-6, "{lengths:?}");
    // A stroke wider than the widest lineweight is a polyline of that width.
    let mut wide = stroke(&[[0.0, 0.0], [10.0, 0.0]], false);
    if let VectorItem::Path(p) = &mut wide {
        p.paint = PathPaint::Stroke(Stroke { width: 10.0, dash: vec![], dash_offset: 0.0 });
    }
    let (dxf, s) = super::dxf(&page(vec![wide]), &opts(Units::Millimetres, 1.0));
    assert_eq!((s.lines, s.polylines), (0, 1));
    assert!((f(get(&entities(&dxf)[0].1, 43)[0]) - 10.0 * 25.4 / 72.0).abs() < 1e-6);
}

#[test]
fn coordinates_are_at_full_size_in_the_drawing_units() {
    // A 3 m wall on a 1:100 sheet is 30 mm long on paper.
    let mm = 72.0 / 25.4;
    let wall = stroke(&[[10.0, 50.0], [10.0 + 30.0 * mm, 50.0]], false);
    for (units, len, insunits) in [(Units::Millimetres, 3000.0, "4"), (Units::Metres, 3.0, "6"), (Units::Centimetres, 300.0, "5")] {
        let (dxf, _) = dxf(&page(vec![wall.clone()]), &opts(units, 100.0));
        let e = entities(&dxf);
        let x0 = f(get(&e[0].1, 10)[0]);
        let x1 = f(get(&e[0].1, 11)[0]);
        assert!((x1 - x0 - len).abs() < 1e-6 * len, "{units:?}: {}", x1 - x0);
        // y up from the bottom of the page.
        assert!((f(get(&e[0].1, 20)[0]) - 50.0 * 25.4 / 72.0 * 100.0 / units.millimetres()).abs() < 1e-6);
        let p = pairs(&dxf);
        let at = p.iter().position(|(c, v)| *c == 9 && v == "$INSUNITS").unwrap();
        assert_eq!(p[at + 1].1, insunits);
    }
}

#[test]
fn text_keeps_its_font_size_angle_and_slant() {
    let mut p = page(vec![VectorItem::Text(TextItem {
        text: "บ้าน 100%%".into(),
        origin: [20.0, 80.0],
        size: 10.0,
        direction: [0.0, -1.0],
        width_factor: 0.8,
        oblique: 15.0,
        length: 40.0,
        color: [0, 0, 0],
        font: 0,
        layer: None,
    })]);
    p.fonts.push(FontFace { family: "TH SarabunPSK".into(), bold: true, italic: false, ascent: 0.85 });
    let (dxf, s) = dxf(&p, &opts(Units::Millimetres, 1.0));
    assert_eq!(s.texts, 1);
    let e = entities(&dxf);
    let t = &e[0].1;
    // CAD programs size TrueType text by its ascent: an em of 10 pt with an ascent of 0.85.
    assert!((f(get(t, 40)[0]) - 8.5 * 25.4 / 72.0).abs() < 1e-9);
    assert_eq!(get(t, 1), ["บ้าน 100%%%%%%"], "a literal %% is escaped");
    assert_eq!(get(t, 50), ["90.0"], "up the page");
    assert_eq!(get(t, 41), ["0.8"]);
    assert_eq!(get(t, 51), ["15.0"]);
    assert_eq!(get(t, 7), ["TH SarabunPSK Bold"]);
    assert!((f(get(t, 20)[0]) - 20.0 * 25.4 / 72.0).abs() < 1e-9);
    let pr = pairs(&dxf);
    let style = records(&pr, "TABLES").into_iter().find(|(k, g)| *k == "STYLE" && g.contains(&(2, "TH SarabunPSK Bold"))).unwrap();
    assert!(style.1.contains(&(3, "THSarabun.ttf")) && style.1.contains(&(1000, "TH SarabunPSK")), "{:?}", style.1);
    assert!(style.1.contains(&(1071, "33554466")), "bold flag on: {:?}", style.1);
}

#[test]
fn layers_come_from_the_pdf_and_otherwise_from_what_was_drawn() {
    let mut walls = stroke(&[[0.0, 0.0], [10.0, 0.0]], false);
    let mut doors = stroke(&[[0.0, 5.0], [10.0, 5.0]], false);
    if let VectorItem::Path(p) = &mut walls {
        p.layer = Some(0);
    }
    if let VectorItem::Path(p) = &mut doors {
        p.layer = Some(1);
    }
    let fill = VectorItem::Path(PathItem {
        subpaths: vec![Subpath { start: [0.0, 0.0], segs: vec![Seg::Line([5.0, 0.0]), Seg::Line([5.0, 5.0])], closed: true }],
        paint: PathPaint::Fill { even_odd: false },
        color: [10, 10, 10],
        layer: None,
    });
    let mut p = page(vec![walls, doors, stroke(&[[0.0, 9.0], [10.0, 9.0]], false), fill]);
    p.layers = vec!["A-WALL: new?".into(), "a-wall: NEW?".into()];
    let (dxf, s) = dxf(&p, &opts(Units::Millimetres, 1.0));
    assert_eq!(s.hatches, 1);
    let layer: Vec<String> = entities(&dxf).iter().map(|(_, g)| get(g, 8)[0].to_string()).collect();
    // Names lose what DXF forbids; names equal but for case are one layer, as in CAD programs.
    assert_eq!(layer, ["A-WALL_ new_", "A-WALL_ new_", "PDF_Geometry", "PDF_Solid Fills"]);
}

#[test]
fn the_white_paper_is_not_a_fill() {
    let paper = VectorItem::Path(PathItem {
        subpaths: vec![Subpath {
            start: [0.0, 0.0],
            segs: vec![Seg::Line([200.0, 0.0]), Seg::Line([200.0, 100.0]), Seg::Line([0.0, 100.0])],
            closed: true,
        }],
        paint: PathPaint::Fill { even_odd: false },
        color: [255, 255, 255],
        layer: None,
    });
    let mut label = paper.clone();
    if let VectorItem::Path(p) = &mut label {
        p.subpaths[0].segs = vec![Seg::Line([20.0, 0.0]), Seg::Line([20.0, 10.0]), Seg::Line([0.0, 10.0])];
    }
    let (dxf, s) = dxf(&page(vec![paper, label]), &opts(Units::Millimetres, 1.0));
    assert_eq!(s.hatches, 1, "a white label background stays");
    assert_eq!(get(&entities(&dxf)[0].1, 420), ["16777215"]);
}

#[test]
fn the_stated_scale_is_read_from_the_page() {
    let text = |s: &str| {
        VectorItem::Text(TextItem {
            text: s.into(),
            origin: [0.0, 0.0],
            size: 10.0,
            direction: [1.0, 0.0],
            width_factor: 1.0,
            oblique: 0.0,
            length: 10.0,
            color: [0, 0, 0],
            font: 0,
            layer: None,
        })
    };
    assert_eq!(stated_scale(&page(vec![text("SCALE 1:100"), text("SCALE"), text("1 : 50"), text("Scale 1:100")])), Some(100.0));
    assert_eq!(stated_scale(&page(vec![text("มาตราส่วน 1:20")])), Some(20.0));
    assert_eq!(stated_scale(&page(vec![text("1:1,000")])), Some(1000.0));
    assert_eq!(stated_scale(&page(vec![text("Room 1:2 B"), text("12.00")])), None, "a ratio that isn't a scale label");
    let mut outlined = page(vec![]);
    outlined.outlined_text = vec!["SCALE 1:50".into()];
    assert_eq!(stated_scale(&outlined), Some(50.0));
}

#[test]
fn numbers_are_written_plainly() {
    assert_eq!(num(0.1 + 0.2), "0.3");
    assert_eq!(num(-0.0), "0.0");
    assert_eq!(num(1e21), "1000000000000000000000.0");
    assert_eq!(num(f64::NAN), "0.0");
    assert_eq!(num(2.0), "2.0");
}

#[test]
fn an_empty_page_is_a_valid_empty_drawing() {
    let (dxf, s) = dxf(&VectorPage::default(), &opts(Units::Millimetres, f64::NAN));
    assert_eq!(s, DxfSummary::default());
    assert!(dxf.ends_with("  0\r\nEOF\r\n"));
    assert!(entities(&dxf).is_empty());
}

#[test]
fn the_active_view_has_one_nonzero_direction_for_empty_small_and_many_layer_drawings() {
    // VPORT's view direction is one 16/26/36 triplet. An extra 36 is not an
    // override: readers may reject it or retain the first, zero-length vector.
    // See Autodesk's VPORT DXF reference, GUID-8CE7CC87-27BD-4490-89DA-C21F516415A9.
    for count in [0, 1, 500] {
        let mut p = page(Vec::new());
        for i in 0..count {
            p.layers.push(format!("ชั้น {i}"));
            let mut item = stroke(&[[10.0, 20.0], [50.0, 20.0]], false);
            if let VectorItem::Path(path) = &mut item {
                path.layer = Some(i);
            }
            p.items.push(item);
        }
        let (drawing, summary) = dxf(&p, &opts(Units::Millimetres, 100.0));
        assert_eq!(summary.lines, count);
        let tags = pairs(&drawing);
        let table_records = records(&tags, "TABLES");
        assert_eq!(table_records.iter().filter(|(kind, _)| *kind == "LAYER").count(), count + 1);
        let views: Vec<_> = table_records.iter().filter(|(kind, _)| *kind == "VPORT").collect();
        assert_eq!(views.len(), 1);
        let view = &views[0].1;
        assert!(view.contains(&(2, "*Active")));
        let direction: Vec<_> = view.iter().filter(|(code, _)| matches!(code, 16 | 26 | 36)).copied().collect();
        assert_eq!(direction, [(16, "0.0"), (26, "0.0"), (36, "1.0")], "{count} layers");
        // All fields this writer emits in a VPORT are single-valued except
        // subclass markers. Catch duplicate scalars before a tolerant importer
        // can silently discard them.
        let mut codes = std::collections::HashSet::new();
        for (code, _) in view {
            assert!(*code == 100 || codes.insert(*code), "duplicate VPORT group {code}");
        }
    }
}
