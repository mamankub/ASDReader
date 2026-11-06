use super::*;

/// A one-page PDF (`media` points) drawing `content`, with `resources` and `extra` page entries;
/// more objects (numbered from 5) come from `objects`.
fn pdf(media: &str, extra: &str, resources: &str, content: &[u8], objects: &[&[u8]]) -> Arc<Vec<u8>> {
    let mut b = asdreader_testkit::Pdf::new()
        .obj("<< /Type /Catalog /Pages 2 0 R >>")
        .obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(format!("<< /Type /Page /Parent 2 0 R /MediaBox [{media}] /Contents 4 0 R /Resources << {resources} >> {extra} >>"))
        .stream("", content);
    for o in objects {
        b = b.obj(o);
    }
    Arc::new(b.build())
}

fn vectors(bytes: Arc<Vec<u8>>, options: VectorOptions) -> VectorPage {
    page_vectors(bytes, &RenderConfig::default(), 0, options).expect("the page reads")
}

fn paths(page: &VectorPage) -> Vec<&PathItem> {
    page.items.iter().filter_map(|i| if let VectorItem::Path(p) = i { Some(p) } else { None }).collect()
}

fn texts(page: &VectorPage) -> Vec<&TextItem> {
    page.items.iter().filter_map(|i| if let VectorItem::Text(t) = i { Some(t) } else { None }).collect()
}

fn close(a: Pt, b: Pt) -> bool {
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

const HELVETICA: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >>";

#[test]
fn strokes_and_fills_keep_their_colour_width_and_shape_in_page_points() {
    let page = vectors(pdf("0 0 200 100", "", "", b"2 w 1 0 0 RG 10 10 m 50 10 l S 0 0 1 rg 10 20 30 30 re f", &[]), VectorOptions::default());
    assert_eq!((page.width, page.height), (200.0, 100.0));
    let p = paths(&page);
    assert_eq!(p.len(), 2, "{p:?}");
    let PathPaint::Stroke(s) = &p[0].paint else { panic!("a stroke: {:?}", p[0].paint) };
    assert!((s.width - 2.0).abs() < 1e-6 && s.dash.is_empty());
    assert_eq!(p[0].color, [255, 0, 0]);
    // y down from the top of the page.
    assert!(close(p[0].subpaths[0].start, [10.0, 90.0]));
    assert_eq!(p[0].subpaths[0].segs, vec![Seg::Line([50.0, 90.0])]);
    assert_eq!(p[1].paint, PathPaint::Fill { even_odd: false });
    assert_eq!(p[1].color, [0, 0, 255]);
    assert!(p[1].subpaths[0].closed);
    assert_eq!(p[1].layer, None);
}

#[test]
fn widths_and_dashes_are_scaled_by_the_transform() {
    let page = vectors(pdf("0 0 200 100", "", "", b"2 0 0 2 0 0 cm 1 w [3 1] 0 d 0 5 m 10 5 l S", &[]), VectorOptions::default());
    let PathPaint::Stroke(s) = &paths(&page)[0].paint else { panic!("a stroke") };
    assert!((s.width - 2.0).abs() < 1e-6, "{s:?}");
    assert_eq!(s.dash, vec![6.0, 2.0]);
}

#[test]
fn a_turned_page_is_read_as_it_is_shown() {
    // /Rotate 90 turns the page clockwise: user (x, y) is shown at (y, x) from the top left.
    let page = vectors(pdf("0 0 200 100", "/Rotate 90", "", b"10 30 m 60 30 l S", &[]), VectorOptions::default());
    assert_eq!((page.width, page.height), (100.0, 200.0));
    let s = &paths(&page)[0].subpaths[0];
    assert!(close(s.start, [30.0, 10.0]), "{s:?}");
    assert!(close(s.segs[0].end(), [30.0, 60.0]), "{s:?}");
}

#[test]
fn what_the_clip_hides_is_cut_away() {
    let content = b"q 0 0 50 100 re W n 0 50 m 100 50 l S 60 60 m 90 60 l S 0 0 1 rg 40 10 20 20 re f Q 70 70 m 80 70 l S";
    let page = vectors(pdf("0 0 200 100", "", "", content, &[]), VectorOptions::default());
    let p = paths(&page);
    assert_eq!(p.len(), 3, "the line wholly outside the clip is gone: {p:?}");
    let line = &p[0].subpaths[0];
    assert!(close(line.start, [0.0, 50.0]) && close(line.segs.last().map(Seg::end).unwrap(), [50.0, 50.0]), "{line:?}");
    // The fill keeps the part inside: x 40..50.
    let xs: Vec<f64> = std::iter::once(p[1].subpaths[0].start).chain(p[1].subpaths[0].segs.iter().map(Seg::end)).map(|q| q[0]).collect();
    assert!(xs.iter().all(|x| (40.0 - 1e-9..=50.0 + 1e-9).contains(x)) && xs.contains(&50.0), "{xs:?}");
    // After the clip is restored, nothing is cut.
    assert!(close(p[2].subpaths[0].start, [70.0, 30.0]));
}

#[test]
fn text_comes_in_runs_on_a_line_with_its_size_and_direction() {
    let content = b"BT /F1 10 Tf 20 50 Td [(Hello) -350 (World)] TJ ET BT /F1 10 Tf 20 20 Td (Far) Tj 200 0 Td (Away) Tj ET \
                    BT /F1 12 Tf 0 1 -1 0 150 10 Tm (Up) Tj ET";
    let page = vectors(pdf("0 0 300 100", "", HELVETICA, content, &[]), VectorOptions::default());
    let t = texts(&page);
    let words: Vec<&str> = t.iter().map(|t| t.text.as_str()).collect();
    // A word gap a little wider than a space reads as one; a column far off starts a new run.
    assert_eq!(words, ["Hello World", "Far", "Away", "Up"], "{t:?}");
    assert!(close(t[0].origin, [20.0, 50.0]) && (t[0].size - 10.0).abs() < 1e-6);
    assert!(close(t[0].direction, [1.0, 0.0]) && (t[0].width_factor - 1.0).abs() < 1e-6 && t[0].oblique.abs() < 1e-6);
    assert!(t[0].length > 40.0 && t[0].length < 70.0, "{}", t[0].length);
    // Up the page: the baseline points to the top (y down).
    assert!(close(t[3].direction, [0.0, -1.0]) && (t[3].size - 12.0).abs() < 1e-6, "{:?}", t[3]);
    let face = &page.fonts[t[0].font];
    assert!(!face.family.is_empty() && face.ascent > 0.5 && face.ascent < 1.5, "{face:?}");
}

#[test]
fn stretched_and_slanted_text_says_so() {
    let content = b"BT /F1 10 Tf 150 Tz 20 50 Td (Wide) Tj ET BT /F1 10 Tf 1 0 0.2679 1 20 20 Tm (Lean) Tj ET";
    let t = vectors(pdf("0 0 300 100", "", HELVETICA, content, &[]), VectorOptions::default());
    let t = texts(&t);
    assert!((t[0].width_factor - 1.5).abs() < 1e-3, "{:?}", t[0]);
    assert!((t[1].oblique - 15.0).abs() < 0.05, "{:?}", t[1]);
}

#[test]
fn text_as_outlines_is_filled_glyphs_and_still_readable() {
    let content = b"BT /F1 10 Tf 20 50 Td (SCALE 1:100) Tj ET";
    let page = vectors(pdf("0 0 300 100", "", HELVETICA, content, &[]), VectorOptions { text_as_outlines: true });
    assert!(texts(&page).is_empty());
    let p = paths(&page);
    assert!(p.len() >= 9 && p.iter().all(|p| p.paint == PathPaint::Fill { even_odd: false }), "{}", p.len());
    assert_eq!(page.outlined_text, ["SCALE 1:100"]);
}

#[test]
fn optional_content_is_on_its_layer_and_hidden_layers_are_left_out() {
    let resources = "/Properties << /oc1 5 0 R /oc2 6 0 R >> /XObject << /Fm 7 0 R >>";
    let content = b"/OC /oc1 BDC 0 0 m 10 0 l S EMC /OC /oc2 BDC 0 5 m 10 5 l S EMC 0 9 m 10 9 l S /Fm Do";
    let objects: [&[u8]; 4] = [
        b"<< /Type /OCG /Name (A-WALL) >>",
        b"<< /Type /OCG /Name (Hidden) >>",
        &asdreader_testkit::stream_obj("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /OC 8 0 R", b"0 20 m 10 20 l S"),
        b"<< /Type /OCG /Name (A-DOOR) >>",
    ];
    let mut bytes = (*pdf("0 0 100 100", "", resources, content, &objects)).clone();
    // The catalogue lists the groups, the second one off.
    let catalog = b"<< /Type /Catalog /Pages 2 0 R >>";
    let with = b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R 8 0 R] /D << /OFF [6 0 R] >> >> >>";
    let at = bytes.windows(catalog.len()).position(|w| w == catalog).unwrap();
    bytes.splice(at..at + catalog.len(), with.iter().copied());
    // The cross-reference offsets after the catalogue moved: let the reader rebuild them.
    let page = vectors(Arc::new(bytes), VectorOptions::default());
    let p = paths(&page);
    let layer = |i: usize| p[i].layer.map(|l| page.layers[l].as_str());
    assert_eq!(p.len(), 3, "the hidden layer's line is not exported: {p:?}");
    assert_eq!([layer(0), layer(1), layer(2)], [Some("A-WALL"), None, Some("A-DOOR")]);
}

#[test]
fn images_are_kept_once_with_where_they_are_placed() {
    let resources = "/XObject << /Im 5 0 R >>";
    let content = b"q 20 0 0 10 30 40 cm /Im Do Q q 20 0 0 10 60 40 cm /Im Do Q";
    let pixels = [255u8, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    let image = asdreader_testkit::stream_obj("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", pixels);
    let page = vectors(pdf("0 0 100 100", "", resources, content, &[&image]), VectorOptions::default());
    assert_eq!(page.images.len(), 1, "drawn twice, kept once");
    let img = &page.images[0];
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(&img.rgba[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
    assert!(img.jpeg.is_none());
    let placed: Vec<&ImageItem> = page.items.iter().filter_map(|i| if let VectorItem::Image(m) = i { Some(m) } else { None }).collect();
    assert_eq!(placed.len(), 2);
    // Pixel space (y down from the top-left) to the page: the image's top-left is at (30, 50).
    let [a, b, c, d, e, f] = placed[0].transform;
    assert!(close([e, f], [30.0, 50.0]) && close([a * 2.0, d * 2.0], [20.0, 10.0]) && b == 0.0 && c == 0.0, "{:?}", placed[0].transform);
    assert_eq!(placed[0].clip, None);
}

#[test]
fn an_unreadable_document_or_missing_page_is_an_error_not_a_panic() {
    assert!(page_vectors(Arc::new(b"not a pdf".to_vec()), &RenderConfig::default(), 0, VectorOptions::default()).is_err());
    let one = pdf("0 0 100 100", "", "", b"", &[]);
    assert!(page_vectors(one, &RenderConfig::default(), 3, VectorOptions::default()).is_err());
}
