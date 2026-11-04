//! Thai "PUA" glyphs: the Private Use Area code points that Windows and Mac Thai fonts (Tahoma,
//! Angsana, TH Sarabun, Cordia…) give their repositioned marks and descender-less consonants, so
//! software without OpenType shaping can still lay Thai out: a tone mark lowered when no upper
//! vowel is above it (เจ้า, บ้าน), shifted left over a tall consonant (ไฟฟ้า, เป็น), a lower vowel
//! dropped under ฎ ฏ, and ญ ฐ without their tails above a lower vowel.
//!
//! PDF producers that embed such a font often write these glyphs' Unicode as the PUA code point
//! (`ToUnicode` maps the glyph to U+F70B, not U+0E49). Read as it is, the text loses those marks:
//! search, copy and the text editor see `เจาของ` instead of `เจ้าของ`. [`standard`] and
//! [`normalize`] give the Thai character each PUA glyph stands for, and [`positioned`] goes the
//! other way, picking the PUA variant the same rules choose, so that text written back with such
//! a font looks as the font's designer intended.
//!
//! The tables are the Windows (U+F700–U+F71A) and Mac (U+F884–U+F89E) conventions; the
//! positioning rules are the standard ones for Thai fallback shaping (consonant classes, then a
//! state machine over the marks above and below each consonant).

use std::borrow::Cow;

/// How a Thai PUA variant is placed relative to its standard glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shift {
    /// Lowered: a tone mark with no upper vowel under it, a lower vowel under ฎ ฏ.
    Down,
    /// Moved left: over the tall stem of ป ฝ ฟ ฬ.
    Left,
    /// Lowered and moved left: a tone mark directly over ป ฝ ฟ ฬ.
    DownLeft,
    /// ญ ฐ without the descender, above a lower vowel.
    NoDescender,
}

/// `(standard, shift, windows PUA, mac PUA)`.
const TABLE: &[(char, Shift, char, char)] = &[
    ('\u{0E48}', Shift::Down, '\u{F70A}', '\u{F88B}'),
    ('\u{0E49}', Shift::Down, '\u{F70B}', '\u{F88E}'),
    ('\u{0E4A}', Shift::Down, '\u{F70C}', '\u{F891}'),
    ('\u{0E4B}', Shift::Down, '\u{F70D}', '\u{F894}'),
    ('\u{0E4C}', Shift::Down, '\u{F70E}', '\u{F897}'),
    ('\u{0E38}', Shift::Down, '\u{F718}', '\u{F89B}'),
    ('\u{0E39}', Shift::Down, '\u{F719}', '\u{F89C}'),
    ('\u{0E3A}', Shift::Down, '\u{F71A}', '\u{F89D}'),
    ('\u{0E48}', Shift::DownLeft, '\u{F705}', '\u{F88C}'),
    ('\u{0E49}', Shift::DownLeft, '\u{F706}', '\u{F88F}'),
    ('\u{0E4A}', Shift::DownLeft, '\u{F707}', '\u{F892}'),
    ('\u{0E4B}', Shift::DownLeft, '\u{F708}', '\u{F895}'),
    ('\u{0E4C}', Shift::DownLeft, '\u{F709}', '\u{F898}'),
    ('\u{0E48}', Shift::Left, '\u{F713}', '\u{F88A}'),
    ('\u{0E49}', Shift::Left, '\u{F714}', '\u{F88D}'),
    ('\u{0E4A}', Shift::Left, '\u{F715}', '\u{F890}'),
    ('\u{0E4B}', Shift::Left, '\u{F716}', '\u{F893}'),
    ('\u{0E4C}', Shift::Left, '\u{F717}', '\u{F896}'),
    ('\u{0E31}', Shift::Left, '\u{F710}', '\u{F884}'),
    ('\u{0E34}', Shift::Left, '\u{F701}', '\u{F885}'),
    ('\u{0E35}', Shift::Left, '\u{F702}', '\u{F886}'),
    ('\u{0E36}', Shift::Left, '\u{F703}', '\u{F887}'),
    ('\u{0E37}', Shift::Left, '\u{F704}', '\u{F888}'),
    ('\u{0E47}', Shift::Left, '\u{F712}', '\u{F889}'),
    ('\u{0E4D}', Shift::Left, '\u{F711}', '\u{F899}'),
    ('\u{0E0D}', Shift::NoDescender, '\u{F70F}', '\u{F89A}'),
    ('\u{0E10}', Shift::NoDescender, '\u{F700}', '\u{F89E}'),
];

/// The Thai character a Thai PUA code point stands for; `None` for anything else.
pub fn standard(c: char) -> Option<char> {
    if !matches!(c, '\u{F700}'..='\u{F71A}' | '\u{F884}'..='\u{F89E}') {
        return None;
    }
    TABLE.iter().find(|(_, _, win, mac)| *win == c || *mac == c).map(|(s, ..)| *s)
}

/// `text` with every Thai PUA code point replaced by the Thai character it stands for. Borrowed
/// (no copy) when there is none, which is nearly always.
pub fn normalize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| standard(c).is_some()) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(|c| standard(c).unwrap_or(c)).collect())
}

/// [`normalize`] for text that may already be owned.
pub fn normalize_cow(text: Cow<'_, str>) -> Cow<'_, str> {
    match normalize(&text) {
        Cow::Borrowed(_) => text,
        Cow::Owned(s) => Cow::Owned(s),
    }
}

/// The PUA variant of `c` for `shift` that `has` accepts, Windows first, then Mac.
fn variant(c: char, shift: Shift, has: &impl Fn(char) -> bool) -> Option<char> {
    TABLE.iter().filter(|(s, sh, ..)| *s == c && *sh == shift).find_map(|(_, _, win, mac)| [*win, *mac].into_iter().find(|v| has(*v)))
}

/// `out[i] = c`; `i` always comes from enumerating a vector of the same length.
fn set(out: &mut [char], i: usize, c: char) {
    if let Some(slot) = out.get_mut(i) {
        *slot = c;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Consonant {
    /// Most consonants.
    Normal,
    /// ป ฝ ฟ ฬ: a tall stem that upper marks must clear.
    Ascender,
    /// ญ ฐ: a descender that a lower vowel replaces.
    Removable,
    /// ฎ ฏ: a descender that a lower vowel goes under.
    Descender,
}

fn consonant(c: char) -> Option<Consonant> {
    match c {
        '\u{0E1B}' | '\u{0E1D}' | '\u{0E1F}' | '\u{0E2C}' => Some(Consonant::Ascender),
        '\u{0E0D}' | '\u{0E10}' => Some(Consonant::Removable),
        '\u{0E0E}' | '\u{0E0F}' => Some(Consonant::Descender),
        '\u{0E01}'..='\u{0E2E}' => Some(Consonant::Normal),
        _ => None,
    }
}

fn is_upper_vowel(c: char) -> bool {
    matches!(c, '\u{0E31}' | '\u{0E34}'..='\u{0E37}' | '\u{0E47}' | '\u{0E4D}' | '\u{0E4E}')
}

fn is_lower_vowel(c: char) -> bool {
    matches!(c, '\u{0E38}'..='\u{0E3A}')
}

fn is_tone(c: char) -> bool {
    matches!(c, '\u{0E48}'..='\u{0E4C}')
}

/// `text` (standard Thai) with marks and consonants swapped for the PUA variants that a font
/// without shaping needs, for each variant `has` says the font can show. Characters the rules
/// leave alone, and variants the font lacks, stay as they are, so the result never asks the font
/// for more than the input did, plus glyphs it is known to have.
///
/// A tone mark is lowered when no upper vowel is under it, unless SARA AM follows (its nikhahit
/// is above the consonant, as in น้ำ); over ป ฝ ฟ ฬ it is also moved left, and so are upper vowels.
/// A lower vowel under ฎ ฏ is lowered; ญ ฐ lose their descender above one. Text that starts with
/// a mark (a part of a word encoded on its own) has no consonant to go by and is left as it is.
pub fn positioned(text: &str, has: impl Fn(char) -> bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = chars.clone();
    // The consonant the marks belong to: its index and class.
    let mut base: Option<(usize, Consonant)> = None;
    // Whether an upper vowel has been seen over the current consonant.
    let mut upper = false;
    for (i, &c) in chars.iter().enumerate() {
        if let Some(class) = consonant(c) {
            base = Some((i, class));
            upper = false;
            continue;
        }
        let Some((at, class)) = base else { continue };
        if is_upper_vowel(c) {
            if class == Consonant::Ascender
                && !upper
                && let Some(v) = variant(c, Shift::Left, &has)
            {
                set(&mut out, i, v);
            }
            upper = true;
        } else if is_tone(c) {
            // SARA AM after the tone puts its nikhahit above the consonant first.
            let am_follows = chars.get(i + 1..).into_iter().flatten().find(|n| !is_tone(**n)) == Some(&'\u{0E33}');
            let shift = match (class, upper) {
                (_, _) if am_follows => None,
                (Consonant::Ascender, false) => Some(Shift::DownLeft),
                (Consonant::Ascender, true) => Some(Shift::Left),
                (_, false) => Some(Shift::Down),
                (_, true) => None,
            };
            if let Some(v) = shift.and_then(|s| variant(c, s, &has)) {
                set(&mut out, i, v);
            }
        } else if is_lower_vowel(c) {
            match class {
                Consonant::Descender => {
                    if let Some(v) = variant(c, Shift::Down, &has) {
                        set(&mut out, i, v);
                    }
                }
                Consonant::Removable => {
                    if let Some(v) = out.get(at).and_then(|b| variant(*b, Shift::NoDescender, &has)) {
                        set(&mut out, at, v);
                    }
                }
                Consonant::Normal | Consonant::Ascender => {}
            }
        } else if c != '\u{0E33}' {
            // Anything else (a following vowel, a space, Latin) ends the consonant's cluster.
            base = None;
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font with every Windows PUA variant.
    fn windows(c: char) -> bool {
        matches!(c, '\u{F700}'..='\u{F71A}')
    }

    #[test]
    fn pua_code_points_read_as_thai() {
        assert_eq!(normalize("เจ\u{F70B}าของ"), "เจ้าของ");
        assert_eq!(normalize("ไฟฟ\u{F706}า เป\u{F712}น ภูมิสถาป\u{F710}ตย ก\u{F70C}อก ณ\u{F70E}"), "ไฟฟ้า เป็น ภูมิสถาปัตย ก๊อก ณ์");
        assert_eq!(normalize("บ\u{F88E}าน"), "บ้าน", "the Mac table too");
        assert!(matches!(normalize("plain ไทย"), Cow::Borrowed(_)));
        assert_eq!(standard('\u{F71B}'), None);
        assert_eq!(standard('\u{E000}'), None);
        // Every variant reads back as its own standard character.
        for (s, _, win, mac) in TABLE {
            assert_eq!((standard(*win), standard(*mac)), (Some(*s), Some(*s)));
        }
    }

    /// The contexts in a real PDFsharp drawing set: each variant the rules choose is the one the
    /// producer wrote.
    #[test]
    fn marks_take_the_variant_their_consonant_needs() {
        for (text, expected) in [
            ("เจ้าของ", "เจ\u{F70B}าของ"),
            ("บ้าน", "บ\u{F70B}าน"),
            ("ไฟฟ้า", "ไฟฟ\u{F706}า"),
            ("เป็น", "เป\u{F712}น"),
            ("เปิด", "เป\u{F701}ด"),
            ("สถาปัตย", "สถาป\u{F710}ตย"),
            ("ก๊อก", "ก\u{F70C}อก"),
            ("ศูนย์", "ศูนย\u{F70E}"),
            // A tone over an upper vowel stays high, except over ป ฝ ฟ ฬ where it moves left.
            ("ที่ตั้ง", "ที่ตั้ง"),
            ("ปั้น", "ป\u{F710}\u{F714}น"),
            // SARA AM: the nikhahit is above, so the tone stays high.
            ("น้ำ", "น้ำ"),
            // Lower vowels under ฎ ฏ go down; ญ ฐ lose the tail.
            ("ฎุ", "ฎ\u{F718}"),
            ("ญู", "\u{F70F}ู"),
            ("ฐุ", "\u{F700}ุ"),
        ] {
            assert_eq!(positioned(text, windows), expected, "{text}");
            assert_eq!(normalize(&positioned(text, windows)), text, "{text} round trip");
        }
    }

    #[test]
    fn missing_variants_and_loose_marks_are_left_alone() {
        assert_eq!(positioned("เจ้าของ", |_| false), "เจ้าของ");
        assert_eq!(positioned("เจ้าของ", |c| c == '\u{F88E}'), "เจ\u{F88E}าของ", "only the Mac variant");
        // A piece of a word encoded on its own: no consonant to position against.
        assert_eq!(positioned("้า", windows), "้า");
        assert_eq!(positioned("", windows), "");
        assert_eq!(positioned("Latin 123", windows), "Latin 123");
        // A space ends the cluster.
        assert_eq!(positioned("ก ้", windows), "ก ้");
    }
}
