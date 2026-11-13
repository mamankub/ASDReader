//! A simple font whose program is not embedded asks the resolver for itself by name.
//!
//! hayro used to decide alone that every non-embedded TrueType/Type1 font is one of its 14 bundled
//! stand-ins (Arial becomes Helvetica, Tahoma becomes Helvetica, Angsana becomes Times), so no
//! resolver could ever supply the real face. These tests pin the hook: the resolver is asked first,
//! with the font's own name, and when it declines the 14-stand-in drawing is unchanged.

use std::sync::{Arc, Mutex};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::font::{FontData, FontQuery};
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};

/// A Latin face that is already in the repository (assets/fonts, AGENTS.md §1.4). Stands in for
/// "the real Tahoma installed on the user's machine".
const INTER: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");

/// One page, one non-embedded TrueType font named `base_font`, the text `Hamburgefonstiv 123`.
fn pdf_with_non_embedded_truetype(base_font: &str, descriptor_extra: &str) -> Vec<u8> {
    let widths = vec!["500"; 95].join(" ");
    let content = b"BT /F1 24 Tf 1 0 0 1 10 20 Tm (Hamburgefonstiv 123) Tj ET";
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 360 60] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n", content.len()).into_bytes(), content.to_vec(), b"\nendstream".to_vec()].concat(),
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /{base_font} /FirstChar 32 /LastChar 126 /Widths [{widths}] \
             /Encoding /WinAnsiEncoding /FontDescriptor << /Type /FontDescriptor /FontName /{base_font} /Flags 32 \
             /FontBBox [-500 -300 1500 1000] /ItalicAngle 0 /Ascent 900 /Descent -250 /CapHeight 700 /StemV 80 {descriptor_extra} >> >>"
        )
        .into_bytes(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offs {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

type Resolver = Arc<dyn Fn(&FontQuery) -> Option<(FontData, u32)> + Send + Sync>;

fn render(pdf: &[u8], resolver: Resolver) -> Vec<u8> {
    let parsed = Pdf::new(Arc::new(pdf.to_vec())).expect("the synthetic PDF parses");
    let pages = parsed.pages();
    let page = pages.first().expect("one page");
    let settings = InterpreterSettings { font_resolver: resolver, ..InterpreterSettings::default() };
    let rs = RenderSettings { width: Some(360), height: Some(60), bg_color: WHITE, ..Default::default() };
    hayro::render(page, &RenderCache::new(), &settings, &rs).data_as_u8_slice().to_vec()
}

fn dark_pixels(rgba: &[u8]) -> usize {
    rgba.as_chunks::<4>().0.iter().filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128).count()
}

/// hayro's own resolver, for every query a test does not care about.
fn standard() -> Resolver {
    InterpreterSettings::default().font_resolver
}

#[test]
fn a_non_embedded_font_asks_the_resolver_for_its_own_name() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let std_resolver = standard();
    let log = Arc::clone(&seen);
    let resolver: Resolver = Arc::new(move |q| {
        if let FontQuery::Fallback(f) = q {
            // A simple font has no character collection; CID fonts do.
            if !f.is_composite
                && let Ok(mut s) = log.lock()
            {
                s.push(f.post_script_name.clone().unwrap_or_default());
            }
        }
        std_resolver(q)
    });
    let pdf = pdf_with_non_embedded_truetype("Tahoma", "");
    let _ = render(&pdf, resolver);
    let names = seen.lock().map(|s| s.clone()).unwrap_or_default();
    assert!(names.iter().any(|n| n == "Tahoma"), "the resolver was never asked for Tahoma by name; it saw {names:?}");
}

#[test]
fn a_resolver_that_supplies_the_face_changes_what_is_drawn() {
    let pdf = pdf_with_non_embedded_truetype("Tahoma", "");
    let std_resolver = standard();
    let baseline = render(&pdf, Arc::clone(&std_resolver));
    assert!(dark_pixels(&baseline) > 200, "the baseline draws ink");

    let std_inner = Arc::clone(&std_resolver);
    let with_face: Resolver = Arc::new(move |q| match q {
        FontQuery::Fallback(f) if !f.is_composite && f.post_script_name.as_deref() == Some("Tahoma") => {
            let data: FontData = Arc::new(INTER.to_vec());
            Some((data, 0))
        }
        _ => std_inner(q),
    });
    let drawn = render(&pdf, with_face);
    assert!(dark_pixels(&drawn) > 200, "the supplied face draws ink");
    assert!(drawn != baseline, "the supplied face must replace the Helvetica stand-in");
}

#[test]
fn a_resolver_that_declines_leaves_the_standard_14_drawing_untouched() {
    let pdf = pdf_with_non_embedded_truetype("Tahoma", "");
    let baseline = render(&pdf, standard());
    let std_inner = standard();
    let declines: Resolver = Arc::new(move |q| match q {
        FontQuery::Fallback(f) if !f.is_composite => None,
        _ => std_inner(q),
    });
    assert!(render(&pdf, declines) == baseline, "declining must not change a single pixel");
}

#[test]
fn an_exact_standard_14_name_is_never_sent_to_the_resolver_by_name() {
    // Helvetica is the 14-font itself, not a stand-in: its metrics are exact, so it keeps its own path.
    let seen: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let std_resolver = standard();
    let log = Arc::clone(&seen);
    let resolver: Resolver = Arc::new(move |q| {
        if let FontQuery::Fallback(f) = q
            && !f.is_composite
            && let Ok(mut n) = log.lock()
        {
            *n += 1;
        }
        std_resolver(q)
    });
    let pdf = pdf_with_non_embedded_truetype("Helvetica", "");
    let _ = render(&pdf, resolver);
    assert_eq!(seen.lock().map(|n| *n).unwrap_or(usize::MAX), 0);
}

// ---- the app's own renderer, through the provider of installed faces -------------------------

use asdreader_fonts::named::{FaceRequest, NamedFace};
use asdreader_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

/// What the desktop app's provider does with this machine's fonts, for two invented names: one
/// that has a face installed (Inter stands in for it) and one whose "face" is not a font program.
/// Every other name has nothing installed, as on a machine without that font.
fn provider(request: &FaceRequest) -> Option<NamedFace> {
    match request.post_script_name.as_str() {
        "ASDInstalledFace" => Some(NamedFace { bytes: Arc::new(INTER.to_vec()), index: 0 }),
        "ASDBrokenFace" => Some(NamedFace { bytes: Arc::new(b"this is not a font program".to_vec()), index: 0 }),
        _ => None,
    }
}

fn app_render(base_font: &str) -> Vec<u8> {
    asdreader_fonts::named::set_named_face_provider(provider);
    let pdf = pdf_with_non_embedded_truetype(base_font, "");
    let mut renderer = PageRenderer::new(Arc::new(pdf), RenderConfig::default());
    let out = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, tile: None, scale: 1.0, tag: 0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    out.rgba.to_vec()
}

#[test]
fn the_app_draws_the_installed_face_a_font_is_named_after() {
    let stand_in = app_render("ASDNothingInstalled");
    let installed = app_render("ASDInstalledFace");
    assert!(dark_pixels(&stand_in) > 200 && dark_pixels(&installed) > 200, "both draw ink");
    assert!(installed != stand_in, "the installed face must replace the bundled stand-in");
}

#[test]
fn a_font_with_no_installed_face_keeps_the_stand_in_whatever_its_name() {
    // Two names with nothing installed draw the same stand-in: the name alone changes nothing.
    assert!(app_render("ASDNothingInstalled") == app_render("ASDAlsoNothingInstalled"));
}

#[test]
fn an_installed_face_that_is_not_a_font_program_leaves_the_stand_in() {
    assert!(app_render("ASDBrokenFace") == app_render("ASDNothingInstalled"), "garbage bytes must not blank or break the page");
}

// ---- weights the name or descriptor spells but hayro did not read ---------------------------------

/// The standard-14 stand-in hayro asks for when the resolver has no face for the font: its debug
/// name, e.g. `Helvetica`, `HelveticaBold`.
fn stand_in_asked_for(base_font: &str, descriptor_extra: &str) -> String {
    let asked: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let std_resolver = standard();
    let log = Arc::clone(&asked);
    let resolver: Resolver = Arc::new(move |q| {
        if let FontQuery::Standard(s) = q
            && let Ok(mut v) = log.lock()
        {
            v.push(format!("{s:?}"));
        }
        std_resolver(q)
    });
    let _ = render(&pdf_with_non_embedded_truetype(base_font, descriptor_extra), resolver);
    let names = asked.lock().map(|v| v.clone()).unwrap_or_default();
    names.last().cloned().unwrap_or_default()
}

#[test]
fn heavy_and_black_weights_get_the_bold_stand_in() {
    // `Arial-Black` and the like are heavier than bold; hayro only knew the word `bold`, so they
    // were drawn in the regular stand-in stretched wide.
    for name in ["Arial-Black", "ArialBlack", "Arial,Black", "Helvetica-Heavy", "Tahoma-Heavy", "ArialNarrow-Bold", "Arial-SemiBold"] {
        assert_eq!(stand_in_asked_for(name, ""), "HelveticaBold", "{name}");
    }
}

#[test]
fn a_descriptor_weight_of_600_or_more_is_bold_and_less_is_not() {
    assert_eq!(stand_in_asked_for("Calibri", "/FontWeight 600"), "HelveticaBold");
    assert_eq!(stand_in_asked_for("Calibri", "/FontWeight 700"), "HelveticaBold");
    assert_eq!(stand_in_asked_for("Calibri", "/FontWeight 500"), "Helvetica");
    assert_eq!(stand_in_asked_for("Calibri", ""), "Helvetica");
}

#[test]
fn a_name_that_merely_contains_black_is_not_made_bold() {
    // `Blackadder ITC` is a script family, not a weight of anything.
    for name in ["BlackadderITC", "Blackberry", "ArialMT", "Arial"] {
        assert_eq!(stand_in_asked_for(name, ""), "Helvetica", "{name}");
    }
}

// ---- composite (Type 0) fonts are told apart by a flag, not by a guess --------------------------

/// A page showing the CID codes `codes` with a non-embedded Type 0 font named `base_font`. Its
/// `/ToUnicode` maps the codes to `text`. `system_info` is the descendant's `/CIDSystemInfo`, or
/// nothing at all (a file that omits what the spec requires).
fn type0_pdf(base_font: &str, text: &str, system_info: bool) -> Vec<u8> {
    let codes: String = (1..=text.chars().count()).map(|i| format!("{i:04X}")).collect();
    let bf: String = text.chars().enumerate().map(|(i, c)| format!("<{:04X}> <{:04X}>\n", i + 1, c as u32)).collect();
    let cmap = format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n/CMapName /T def /CMapType 2 def\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n{} beginbfchar\n{bf}endbfchar\nendcmap end end",
        text.chars().count()
    );
    let content = format!("BT /F1 24 Tf 1 0 0 1 10 20 Tm <{codes}> Tj ET");
    let info = if system_info { "/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> " } else { "" };
    let stream = |s: &str| format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len()).into_bytes();
    let objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 360 60] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_vec(),
        stream(&content),
        format!("<< /Type /Font /Subtype /Type0 /BaseFont /{base_font} /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 8 0 R >>").into_bytes(),
        format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{base_font} {info}/DW 600 /FontDescriptor << /Type /FontDescriptor /FontName /{base_font} \
             /Flags 4 /FontBBox [-500 -300 1500 1000] /ItalicAngle 0 /Ascent 900 /Descent -250 /CapHeight 700 /StemV 80 >> >>"
        )
        .into_bytes(),
        b"null".to_vec(),
        stream(&cmap),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offs {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

#[test]
fn a_type0_font_without_a_cid_system_info_is_still_a_composite_font_and_still_drawn() {
    // `/CIDSystemInfo` is required, but files without it exist; such a font has no character
    // collection, and must neither be taken for a simple font nor lose its stand-in.
    let seen: Arc<Mutex<Vec<(bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let std_resolver = standard();
    let log = Arc::clone(&seen);
    let resolver: Resolver = Arc::new(move |q| {
        if let FontQuery::Fallback(f) = q
            && let Ok(mut v) = log.lock()
        {
            v.push((f.is_composite, f.character_collection.is_some()));
        }
        std_resolver(q)
    });
    let pixels = render(&type0_pdf("SomeCidFont", "ABC", false), resolver);
    let asked = seen.lock().map(|v| v.clone()).unwrap_or_default();
    assert!(asked.iter().any(|&(composite, _)| composite), "a Type 0 font is asked for as a composite font: {asked:?}");
    assert!(dark_pixels(&pixels) > 50, "its stand-in still draws");
}

// ---- a Thai-capable composite font nobody has installed is drawn with a Thai face ----------------

#[test]
fn a_thai_composite_font_with_no_installed_face_is_drawn_with_the_bundled_thai_face() {
    if asdreader_fonts::document_thai_font().is_none() {
        // Built without asdesign-fonts (AGENTS.md §1.4); CI always builds with it.
        return;
    }
    let thai = "\u{0e01}\u{0e02}\u{0e04}\u{0e07}AB1";
    let named = app_render_bytes(&type0_pdf("CordiaNew", thai, true));
    let unrelated = app_render_bytes(&type0_pdf("ASDNoSuchFamily", thai, true));
    assert!(dark_pixels(&named) > 300, "the Thai face draws ink");
    assert!(named != unrelated, "a Thai family is drawn with the Thai face; an unrelated name keeps its stand-in");
    // The same name as a SIMPLE font is not a Thai text font: it is left to the stand-in.
    assert!(
        app_render_bytes(&pdf_with_non_embedded_truetype("CordiaNew", ""))
            == app_render_bytes(&pdf_with_non_embedded_truetype("ASDNoSuchFamily", "")),
        "a simple font keeps its stand-in whatever its name"
    );
}

fn app_render_bytes(pdf: &[u8]) -> Vec<u8> {
    asdreader_fonts::named::set_named_face_provider(provider);
    let mut renderer = PageRenderer::new(Arc::new(pdf.to_vec()), RenderConfig::default());
    let out = renderer.render(RenderRequest { page: 0, kind: RequestKind::Pixels, tile: None, scale: 1.0, tag: 0, ..Default::default() });
    assert!(out.error.is_none(), "{:?}", out.error);
    out.rgba.to_vec()
}
