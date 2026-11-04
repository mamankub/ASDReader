//! Repair of the text a PDF's embedded fonts claim to show, for PDFs written by Microsoft Print To
//! PDF (the engine behind the "ASD PDF Printer" queue) and writers that behave the same way.
//!
//! Those writers draw glyph ids, and give `/ToUnicode` a text for each glyph the page uses. A glyph
//! that no character maps to (a Thai tone mark lowered by the font's GSUB, say) gets an invented
//! code point, so a searched, copied or edited "ที่ตั้ง" is "ที[U+047A]ตั[U+047B]ง". The embedded
//! font program still has what is needed to say what the glyph is ([`GlyphText`]); this module
//! rewrites each such `/ToUnicode` and leaves every mapping the font supports alone.
//!
//! Only composite fonts with an embedded TrueType program and an identity glyph map are touched:
//! there the code is the glyph id, which is what makes the font program's tables answer for it.

use std::fmt::Write as _;

use asdreader_cos::{Dict, Document, ObjRef, Object, Stream};

use crate::glyph_text::GlyphText;
use crate::pdf::Metrics;

/// Most ToUnicode entries one font is rewritten with.
const MAX_ENTRIES: usize = 70_000;
/// Most fonts looked at in one document.
const MAX_FONTS: usize = 4_096;
/// Entries per `beginbfchar` block (the CMap format allows at most 100).
const BLOCK: usize = 100;

/// What a repair did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepairReport {
    /// Fonts looked at that could have been repaired.
    pub fonts_checked: usize,
    /// Fonts whose `/ToUnicode` was rewritten.
    pub fonts_repaired: usize,
    /// Glyph mappings replaced.
    pub glyphs_repaired: usize,
}

impl RepairReport {
    /// Whether the document changed.
    pub fn changed(&self) -> bool {
        self.glyphs_repaired > 0
    }
}

/// Whether the document has a font this repair could act on, cheaply: some composite font with a
/// `/ToUnicode` and an embedded TrueType program (see the module notes). The producer string says
/// little, so the repair is offered for any such font and does nothing when all is well.
pub fn repair_thai_text(doc: &mut Document) -> RepairReport {
    let mut report = RepairReport::default();
    let fonts: Vec<(ObjRef, Dict)> = doc
        .scan_objects()
        .filter_map(|(r, o)| match o.as_ref() {
            Object::Dict(d) if d.name(b"Subtype") == Some(b"Type0") && d.get(b"ToUnicode").is_some() => Some((r, d.clone())),
            _ => None,
        })
        .take(MAX_FONTS)
        .collect();
    for (obj, font) in fonts {
        let Some(program) = crate::pdf::embedded_sfnt(doc, &font) else { continue };
        if !identity_glyph_map(doc, &font) {
            continue;
        }
        let Some(glyphs) = GlyphText::from_font(&program) else { continue };
        report.fonts_checked += 1;
        let metrics = Metrics::from_dict(doc, &font);
        let mut entries = metrics.to_unicode_entries(MAX_ENTRIES);
        let mut fixed = 0usize;
        for (code, written) in &mut entries {
            if let Some(text) = glyphs.text(*code).filter(|t| !glyphs.explains(*code, written) && *t != written.as_str()) {
                *written = text.to_owned();
                fixed += 1;
            }
        }
        // The glyphs a subset carries that the writer left out of /ToUnicode altogether (a font embedded
        // whole has outlines for every glyph, so there is no telling which the page uses).
        if glyphs.is_subset() {
            let have: std::collections::HashSet<u32> = entries.iter().map(|(c, _)| *c).collect();
            for (gid, text) in glyphs.outlined_texts() {
                if !have.contains(&gid) && entries.len() < MAX_ENTRIES {
                    entries.push((gid, text.to_owned()));
                    fixed += 1;
                }
            }
        }
        entries.sort_by_key(|(c, _)| *c);
        if fixed == 0 {
            continue;
        }
        let cmap = to_unicode_cmap(&entries);
        let stream = doc.add(Object::Stream(Stream::flate(Dict::new(), cmap.as_bytes())));
        if doc.update_dict(obj, |d| d.set(b"ToUnicode".to_vec(), Object::Ref(stream))).is_ok() {
            report.fonts_repaired += 1;
            report.glyphs_repaired += fixed;
        }
    }
    report
}

/// Whether the font's glyph map is the identity (the code of a glyph is its id).
fn identity_glyph_map(doc: &Document, font: &Dict) -> bool {
    let descendant = font.get(b"DescendantFonts").map(|o| doc.resolve(o)).and_then(|o| o.as_array().and_then(|a| a.first()).cloned());
    let Some(descendant) = descendant.and_then(|d| doc.dict(&d)) else { return false };
    if descendant.name(b"Subtype") != Some(b"CIDFontType2") {
        return false;
    }
    match descendant.get(b"CIDToGIDMap").map(|o| doc.resolve(o)) {
        None => true,
        Some(o) => matches!(o.as_ref(), Object::Name(n) if n == b"Identity"),
    }
}

/// A ToUnicode CMap holding exactly `entries`, with two-byte codes.
fn to_unicode_cmap(entries: &[(u32, String)]) -> String {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for block in entries.chunks(BLOCK) {
        let _ = writeln!(out, "{} beginbfchar", block.len());
        for (code, text) in block {
            let _ = write!(out, "<{:04X}> <", code & 0xFFFF);
            for unit in text.encode_utf16() {
                let _ = write!(out, "{unit:04X}");
            }
            out.push_str(">\n");
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Type 0 font embedding Inter, whose ToUnicode says what `entries` say.
    fn pdf_with(entries: &str) -> (Document, ObjRef) {
        let inter = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let mut doc = Document::new_empty();
        let program = doc.add(Object::Stream(Stream::from_raw(Dict::new(), inter.to_vec())));
        let mut descriptor = Dict::new();
        descriptor.set(b"FontFile2".to_vec(), Object::Ref(program));
        descriptor.set(b"FontName".to_vec(), Object::name("CIDFont+F1"));
        let mut cid = Dict::new();
        cid.set(b"Subtype".to_vec(), Object::name("CIDFontType2"));
        cid.set(b"CIDToGIDMap".to_vec(), Object::name("Identity"));
        cid.set(b"FontDescriptor".to_vec(), Object::Dict(descriptor));
        let cmap = format!("1 begincodespacerange <0000> <FFFF> endcodespacerange\n{entries}");
        let tu = doc.add(Object::Stream(Stream::from_raw(Dict::new(), cmap.into_bytes())));
        let mut font = Dict::new();
        font.set(b"Type".to_vec(), Object::name("Font"));
        font.set(b"Subtype".to_vec(), Object::name("Type0"));
        font.set(b"BaseFont".to_vec(), Object::name("CIDFont+F1"));
        font.set(b"Encoding".to_vec(), Object::name("Identity-H"));
        font.set(b"DescendantFonts".to_vec(), Object::Array(vec![Object::Dict(cid)]));
        font.set(b"ToUnicode".to_vec(), Object::Ref(tu));
        let obj = doc.add(Object::Dict(font));
        (doc, obj)
    }

    fn glyph(c: char) -> u32 {
        use skrifa::MetadataProvider as _;
        let inter = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        skrifa::FontRef::new(inter).unwrap().charmap().map(c).unwrap().to_u32()
    }

    fn text_of(doc: &Document, obj: ObjRef, gid: u32) -> Option<String> {
        let font = doc.dict(&Object::Ref(obj)).unwrap();
        Metrics::from_dict(doc, &font).text_of(gid).map(|t| t.into_owned())
    }

    /// A PDF printed through Microsoft Print To PDF, in `ASDREADER_REPAIR_PDF`: what the repair does.
    #[test]
    #[ignore = "needs ASDREADER_REPAIR_PDF"]
    fn repair_a_real_printed_pdf() {
        let bytes = std::fs::read(std::env::var("ASDREADER_REPAIR_PDF").unwrap()).unwrap();
        let mut doc = Document::open(std::sync::Arc::new(bytes)).unwrap();
        let fonts: Vec<_> = doc
            .scan_objects()
            .filter_map(|(r, o)| match o.as_ref() {
                Object::Dict(d) if d.name(b"Subtype") == Some(b"Type0") => Some((r, d.clone())),
                _ => None,
            })
            .collect();
        eprintln!("type0 fonts: {}", fonts.len());
        for (r, f) in &fonts {
            eprintln!(
                "  {r:?}: tounicode {} sfnt {} identity {}",
                f.get(b"ToUnicode").is_some(),
                crate::pdf::embedded_sfnt(&doc, f).is_some(),
                identity_glyph_map(&doc, f)
            );
        }
        eprintln!("{:?}", repair_thai_text(&mut doc));
    }

    #[test]
    fn an_invented_code_point_is_replaced_by_the_text_the_font_supports() {
        let (a, b) = (glyph('A'), glyph('B'));
        // A is mapped to the kind of code point Microsoft Print To PDF invents; B is mapped soundly.
        let (mut doc, obj) = pdf_with(&format!("2 beginbfchar <{a:04X}> <047B> <{b:04X}> <0042> endbfchar"));
        assert_eq!(text_of(&doc, obj, a).as_deref(), Some("\u{047B}"));
        let report = repair_thai_text(&mut doc);
        assert_eq!(report, RepairReport { fonts_checked: 1, fonts_repaired: 1, glyphs_repaired: 1 });
        assert!(report.changed());
        assert_eq!(text_of(&doc, obj, a).as_deref(), Some("A"));
        assert_eq!(text_of(&doc, obj, b).as_deref(), Some("B"), "a sound mapping is left as it was");
        // Nothing left to repair: the second run changes nothing.
        assert_eq!(repair_thai_text(&mut doc), RepairReport { fonts_checked: 1, fonts_repaired: 0, glyphs_repaired: 0 });
    }

    #[test]
    fn a_document_without_such_fonts_is_left_alone() {
        let mut doc = Document::new_empty();
        assert_eq!(repair_thai_text(&mut doc), RepairReport::default());
        // A font whose mappings are all sound.
        let a = glyph('A');
        let (mut doc, obj) = pdf_with(&format!("1 beginbfchar <{a:04X}> <0041> endbfchar"));
        let report = repair_thai_text(&mut doc);
        assert_eq!((report.fonts_checked, report.glyphs_repaired), (1, 0));
        assert_eq!(text_of(&doc, obj, a).as_deref(), Some("A"));
    }

    #[test]
    fn the_rewritten_cmap_round_trips_supplementary_text_and_many_entries() {
        let entries: Vec<(u32, String)> = (0..250u32).map(|k| (0x100 + k, if k == 7 { "\u{1F600}".to_owned() } else { "x".to_owned() })).collect();
        let text = to_unicode_cmap(&entries);
        assert_eq!(text.matches("beginbfchar").count(), 3, "at most 100 per block");
        assert!(text.contains("<0107> <D83DDE00>"));
    }
}
