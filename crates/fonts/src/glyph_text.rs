//! The text each glyph of a font program stands for, recovered from the program itself.
//!
//! A PDF writer that draws text with glyph ids (Microsoft Print To PDF, PDFsharp, most CAD
//! exporters) must say in `/ToUnicode` what text each glyph shows. For a glyph that no character
//! maps to (a Thai tone mark lowered or shifted left by the font's GSUB, a vowel with its tail
//! removed, a ligature) some writers invent a code point instead: the lowered mai tho comes out
//! as U+047B, and "ที่ตั้ง" is read as "ที[U+047A]ตั[U+047B]ง". The font program, embedded whole,
//! still holds the answer: `cmap` names the glyphs of the plain characters, and GSUB says which
//! glyph replaces which (single substitution) or combines which (ligature). Reading that backwards
//! gives the text of every glyph the font can reach from characters.
//!
//! [`GlyphText::from_font`] does that for one font program. [`GlyphText::explains`] says whether
//! the text a writer gave a glyph is one the font itself supports, which is what lets a repair
//! replace only invented mappings and leave sound ones alone, and [`GlyphText::outlined_texts`]
//! lists the glyphs a font subset actually carries, for adding the ones a writer left out of
//! `/ToUnicode` altogether (Microsoft Print To PDF does that for the lowered and shifted marks).

use std::collections::{HashMap, HashSet};

use skrifa::{
    FontRef, MetadataProvider as _,
    raw::{
        TableProvider as _,
        tables::gsub::{SingleSubst, SubstitutionSubtables},
    },
};

/// Most GSUB lookups read from one font: a font program is untrusted input.
const MAX_LOOKUPS: usize = 512;
/// Most substitution passes: chains such as base → lowered → left-shifted settle in two or three.
const MAX_PASSES: usize = 6;
/// Most glyphs whose text is kept.
const MAX_GLYPHS: usize = 70_000;

/// The text of the glyphs of one font program.
#[derive(Debug, Default, Clone)]
pub struct GlyphText {
    /// Glyph id → its text.
    text: HashMap<u32, String>,
    /// Glyph id → the characters the font's `cmap` sends to it.
    cmap: HashMap<u32, Vec<char>>,
    /// The glyphs that have an outline (a subset leaves the others empty).
    outlined: HashSet<u32>,
    /// How many glyphs the font declares.
    glyph_count: u32,
}

impl GlyphText {
    /// Read `bytes` (a TrueType or OpenType program); `None` when it doesn't parse.
    pub fn from_font(bytes: &[u8]) -> Option<Self> {
        let font = FontRef::new(bytes).ok()?;
        let mut cmap: HashMap<u32, Vec<char>> = HashMap::new();
        for (cp, gid) in font.charmap().mappings() {
            let (Some(c), gid) = (char::from_u32(cp), gid.to_u32()) else { continue };
            if gid != 0 && cmap.len() < MAX_GLYPHS {
                cmap.entry(gid).or_default().push(c);
            }
        }
        // The plain character of a glyph several characters share (a no-break space with the space,
        // a Thai vowel with its Unicode-block twin): the Thai or lowest one.
        let mut text: HashMap<u32, String> = HashMap::new();
        for (gid, chars) in &mut cmap {
            chars.sort_by_key(|c| (!matches!(*c, '\u{0E00}'..='\u{0E7F}'), *c));
            if let Some(c) = chars.first() {
                text.insert(*gid, c.to_string());
            }
        }
        let glyph_count = font.maxp().map_or(0, |m| u32::from(m.num_glyphs()));
        let mut me = Self { text, cmap, outlined: HashSet::new(), glyph_count };
        me.read_outlined(&font);
        me.interpolate_thai_runs();
        if let Ok(gsub) = font.gsub() {
            me.read_gsub(&gsub);
        }
        Some(me)
    }

    fn read_outlined(&mut self, font: &FontRef<'_>) {
        let (Ok(loca), Ok(glyf), Ok(maxp)) = (font.loca(None), font.glyf(), font.maxp()) else { return };
        for gid in 0..u32::from(maxp.num_glyphs()).min(MAX_GLYPHS as u32) {
            if let Ok(Some(_)) = loca.get_glyf(skrifa::GlyphId::new(gid), &glyf) {
                self.outlined.insert(gid);
            }
        }
    }

    /// A font subset keeps the `cmap` of the characters the document used and drops the rest, so the
    /// glyph of a character that was not used (the plain ญ, from which a GSUB rule made the
    /// tail-less ญ that was) has no character left. Thai fonts lay their glyphs out in Unicode order,
    /// so between two glyphs whose characters are known, if their glyph ids differ by exactly as much
    /// as their code points, every glyph between them is the character at the same distance. The two
    /// ends prove the run; nothing outside such a run is guessed.
    fn interpolate_thai_runs(&mut self) {
        let mut anchors: Vec<(u32, u32)> = self
            .cmap
            .iter()
            .filter_map(|(gid, chars)| chars.iter().find(|c| matches!(**c, '\u{0E01}'..='\u{0E5B}')).map(|c| (*gid, *c as u32)))
            .collect();
        anchors.sort_unstable();
        for pair in anchors.windows(2) {
            let [(ga, ca), (gb, cb)] = [pair[0], pair[1]];
            if gb <= ga + 1 || gb - ga != cb.wrapping_sub(ca) || cb < ca {
                continue;
            }
            for step in 1..(gb - ga) {
                let (gid, cp) = (ga + step, ca + step);
                // Thai has holes in its block (U+0E3B–0E3E, U+0E5C and up): no glyph is a hole.
                if let Some(c) = char::from_u32(cp).filter(|c| matches!(*c, '\u{0E01}'..='\u{0E3A}' | '\u{0E3F}'..='\u{0E5B}')) {
                    self.text.entry(gid).or_insert_with(|| c.to_string());
                }
            }
        }
    }

    fn read_gsub(&mut self, gsub: &skrifa::raw::tables::gsub::Gsub<'_>) {
        let Ok(lookups) = gsub.lookup_list() else { return };
        let mut conflicted: HashSet<u32> = HashSet::new();
        // Pairs (input glyph, output glyph) of every single substitution, and the ligatures,
        // collected once; the passes then only walk these lists.
        let mut singles: Vec<(u32, u32)> = Vec::new();
        let mut ligatures: Vec<(Vec<u32>, u32)> = Vec::new();
        for lookup in lookups.lookups().iter().take(MAX_LOOKUPS).filter_map(Result::ok) {
            let Ok(subtables) = lookup.subtables() else { continue };
            match subtables {
                SubstitutionSubtables::Single(subs) => {
                    for sub in subs.iter().filter_map(Result::ok) {
                        match sub {
                            SingleSubst::Format1(f) => {
                                let Ok(coverage) = f.coverage() else { continue };
                                let delta = i32::from(f.delta_glyph_id());
                                for input in coverage.iter() {
                                    let out = (i64::from(input.to_u32()) + i64::from(delta)).rem_euclid(1 << 16);
                                    singles.push((input.to_u32(), out as u32));
                                }
                            }
                            SingleSubst::Format2(f) => {
                                let Ok(coverage) = f.coverage() else { continue };
                                for (input, out) in coverage.iter().zip(f.substitute_glyph_ids()) {
                                    singles.push((input.to_u32(), u32::from(out.get().to_u16())));
                                }
                            }
                        }
                    }
                }
                SubstitutionSubtables::Alternate(subs) => {
                    for sub in subs.iter().filter_map(Result::ok) {
                        let Ok(coverage) = sub.coverage() else { continue };
                        for (input, set) in coverage.iter().zip(sub.alternate_sets().iter().filter_map(Result::ok)) {
                            for alt in set.alternate_glyph_ids() {
                                singles.push((input.to_u32(), u32::from(alt.get().to_u16())));
                            }
                        }
                    }
                }
                SubstitutionSubtables::Ligature(subs) => {
                    for sub in subs.iter().filter_map(Result::ok) {
                        let Ok(coverage) = sub.coverage() else { continue };
                        for (first, set) in coverage.iter().zip(sub.ligature_sets().iter().filter_map(Result::ok)) {
                            for lig in set.ligatures().iter().filter_map(Result::ok) {
                                let mut parts = vec![first.to_u32()];
                                parts.extend(lig.component_glyph_ids().iter().map(|g| u32::from(g.get().to_u16())));
                                ligatures.push((parts, u32::from(lig.ligature_glyph().to_u16())));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for _ in 0..MAX_PASSES {
            let mut changed = false;
            for &(input, out) in &singles {
                changed |= self.derive(out, self.text.get(&input).cloned(), &mut conflicted);
            }
            for (parts, out) in &ligatures {
                let joined: Option<String> = parts.iter().map(|p| self.text.get(p).map(String::as_str)).collect();
                changed |= self.derive(*out, joined, &mut conflicted);
            }
            if !changed {
                break;
            }
        }
        // A glyph two different texts both lead to is not known; the writer's own mapping stands.
        for gid in conflicted {
            if !self.cmap.contains_key(&gid) {
                self.text.remove(&gid);
            }
        }
    }

    /// Give glyph `gid` the text `from` unless it has its own character or another text; true when
    /// it learned something new.
    fn derive(&mut self, gid: u32, from: Option<String>, conflicted: &mut HashSet<u32>) -> bool {
        let Some(from) = from.filter(|t| !t.is_empty()) else { return false };
        if self.cmap.contains_key(&gid) || self.text.len() >= MAX_GLYPHS {
            return false;
        }
        match self.text.get(&gid) {
            None => {
                self.text.insert(gid, from);
                true
            }
            Some(known) if *known != from => {
                conflicted.insert(gid);
                false
            }
            Some(_) => false,
        }
    }

    /// Whether the program looks like a subset: few of the glyphs it declares have outlines. A writer
    /// that embeds a whole font has all of them, and there the glyphs the page uses can't be told
    /// apart from the rest.
    pub fn is_subset(&self) -> bool {
        self.outlined.len() <= 2_000 && self.outlined.len().saturating_mul(4) <= self.glyph_count as usize
    }

    /// The glyphs that have an outline and a text, in glyph order: in a subset, the glyphs the document
    /// can show.
    pub fn outlined_texts(&self) -> Vec<(u32, &str)> {
        let mut out: Vec<(u32, &str)> = self.outlined.iter().filter_map(|g| self.text.get(g).map(|t| (*g, t.as_str()))).collect();
        out.sort_unstable_by_key(|(g, _)| *g);
        out
    }

    /// The text of glyph `gid`, when the font tells.
    pub fn text(&self, gid: u32) -> Option<&str> {
        self.text.get(&gid).map(String::as_str)
    }

    /// Whether `written` is a text the font itself supports for glyph `gid`: the text this module
    /// recovered, or a single character that the font's `cmap` sends to that glyph.
    pub fn explains(&self, gid: u32, written: &str) -> bool {
        if self.text(gid) == Some(written) {
            return true;
        }
        let mut chars = written.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => self.cmap.get(&gid).is_some_and(|cs| cs.contains(&c)),
            _ => false,
        }
    }
}

/// `text` with Thai SARA AM written as its two parts put back together and in order: a consonant,
/// the nikhahit (U+0E4D), an optional tone mark, SARA AA (U+0E32) becomes consonant, tone mark,
/// SARA AM (U+0E33). Fonts and PDF writers split SARA AM so that the tone mark can sit over the
/// nikhahit's place; read back, `นํ้า` and `นำ้` are not what anyone types or searches for.
pub fn compose_sara_am(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let tone = |k: usize| chars.get(k).copied().filter(|t| matches!(t, '\u{0E48}'..='\u{0E4B}'));
        if c == '\u{0E4D}' {
            // nikhahit, [tone], aa
            if let Some(t) = tone(i + 1)
                && chars.get(i + 2) == Some(&'\u{0E32}')
            {
                out.push(t);
                out.push('\u{0E33}');
                i += 3;
                continue;
            }
            if chars.get(i + 1) == Some(&'\u{0E32}') {
                out.push('\u{0E33}');
                i += 2;
                continue;
            }
        } else if matches!(c, '\u{0E48}'..='\u{0E4B}') && chars.get(i + 1) == Some(&'\u{0E4D}') && chars.get(i + 2) == Some(&'\u{0E32}') {
            // tone, nikhahit, aa (drawing order)
            out.push(c);
            out.push('\u{0E33}');
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sara_am_is_put_back_together() {
        assert_eq!(compose_sara_am("น\u{0E4D}\u{0E49}\u{0E32}"), "น\u{0E49}\u{0E33}");
        assert_eq!(compose_sara_am("น\u{0E49}\u{0E4D}\u{0E32}"), "น\u{0E49}\u{0E33}");
        assert_eq!(compose_sara_am("ท\u{0E4D}\u{0E32}"), "ท\u{0E33}");
        assert_eq!(compose_sara_am("น้ำ ก\u{0E4D}"), "น้ำ ก\u{0E4D}", "a lone nikhahit stays");
        assert_eq!(compose_sara_am(""), "");
    }

    /// Fonts and glyph lists taken from a PDF printed through Microsoft Print To PDF:
    /// `ASDREADER_GLYPH_FONT` is a font program, `ASDREADER_GLYPH_IDS` the used glyph ids.
    #[test]
    #[ignore = "needs ASDREADER_GLYPH_FONT and ASDREADER_GLYPH_IDS"]
    fn show_the_text_of_used_glyphs() {
        let bytes = std::fs::read(std::env::var("ASDREADER_GLYPH_FONT").unwrap()).unwrap();
        let ids = std::env::var("ASDREADER_GLYPH_IDS").unwrap();
        let text = GlyphText::from_font(&bytes).unwrap();
        let font = FontRef::new(&bytes).unwrap();
        let names: Vec<(u32, String)> = font.glyph_names().iter().map(|(g, n)| (g.to_u32(), n.to_string())).collect();
        let name_of = |gid: u32| names.iter().find(|(g, _)| *g == gid).map(|(_, n)| n.clone());
        for gid in std::fs::read_to_string(ids).unwrap().split(',').filter_map(|g| g.parse::<u32>().ok()) {
            if text.text(gid).is_none() {
                eprintln!("UNRESOLVED {gid} name {:?}", name_of(gid));
                if let Ok(gsub) = font.gsub() {
                    for (n, lookup) in gsub.lookup_list().unwrap().lookups().iter().filter_map(Result::ok).enumerate() {
                        match lookup.subtables().unwrap() {
                            SubstitutionSubtables::Multiple(subs) => {
                                for sub in subs.iter().filter_map(Result::ok) {
                                    for (input, seq) in sub.coverage().unwrap().iter().zip(sub.sequences().iter().filter_map(Result::ok)) {
                                        let outs: Vec<u32> = seq.substitute_glyph_ids().iter().map(|g| u32::from(g.get().to_u16())).collect();
                                        if outs.contains(&gid) {
                                            eprintln!(
                                                "  lookup {n}: multiple {} ({:?}) -> {outs:?} {:?}",
                                                input.to_u32(),
                                                name_of(input.to_u32()),
                                                outs.iter().map(|o| text.text(*o).map(str::to_owned)).collect::<Vec<_>>()
                                            );
                                        }
                                    }
                                }
                            }
                            SubstitutionSubtables::Single(subs) => {
                                for sub in subs.iter().filter_map(Result::ok) {
                                    if let SingleSubst::Format2(f) = sub {
                                        for (input, out) in f.coverage().unwrap().iter().zip(f.substitute_glyph_ids()) {
                                            if u32::from(out.get().to_u16()) == gid {
                                                eprintln!(
                                                    "  lookup {n}: single {} ({:?}) {:?} -> {gid}",
                                                    input.to_u32(),
                                                    name_of(input.to_u32()),
                                                    text.text(input.to_u32())
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            eprintln!(
                "{gid:5} {:#06x} {:?}",
                gid,
                text.text(gid).map(|t| t.chars().map(|c| format!("U+{:04X}", c as u32)).collect::<Vec<_>>().join(" "))
            );
        }
    }

    #[test]
    fn a_plain_font_gives_its_character_text_and_explains_only_that() {
        let inter = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let font = FontRef::new(inter).unwrap();
        let a = font.charmap().map('A').unwrap().to_u32();
        let text = GlyphText::from_font(inter).unwrap();
        assert_eq!(text.text(a), Some("A"));
        assert!(text.explains(a, "A"));
        assert!(!text.explains(a, "\u{047B}"), "an invented code point is not explained by the font");
        assert_eq!(text.text(u32::MAX), None);
        assert!(GlyphText::from_font(b"not a font").is_none());
    }
}
