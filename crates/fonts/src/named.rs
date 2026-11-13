//! Installed faces asked for by the name a PDF gives them.
//!
//! A PDF may name a font and not carry its program (`/BaseFont /ArialMT`, `/Tahoma-Bold`,
//! `/AngsanaNew`): CAD and Office exports do it all the time. The only way to draw such a page as
//! its author saw it is the face of that name that is installed on the reader's machine, which is
//! what Acrobat and Foxit do. Without it the renderer can only draw a bundled stand-in (Helvetica,
//! Times, Courier) stretched to the document's widths.
//!
//! This module is the logic and the hook, with no file access: the desktop app names a [`Provider`]
//! with [`set_named_face_provider`] (as it names the Thai one in [`crate::installed`]) that reads
//! this machine's fonts. Without a provider (the CLI, the web build, tests) every lookup is `None`
//! and the stand-in is drawn, so automation output never depends on the machine it runs on.
//!
//! Like every installed font (AGENTS.md §1.4), a face found this way is only drawn with: it is
//! never embedded in a document, shipped, copied or committed.

use std::sync::{Arc, OnceLock};

/// What a PDF font dictionary asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceRequest {
    /// `/BaseFont` with any `ABCDEF+` subset tag stripped: `ArialMT`, `Arial,Bold`, `Tahoma-Bold`.
    pub post_script_name: String,
    /// `/FontDescriptor /FontFamily`, when the document gives one.
    pub family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// `/FontDescriptor /FontWeight` (400 when absent).
    pub weight: u32,
}

/// A face's program as read from where it is installed. `index` is the face within a collection
/// (`.ttc`), 0 for a single font.
#[derive(Clone, Debug)]
pub struct NamedFace {
    pub bytes: Arc<Vec<u8>>,
    pub index: u32,
}

/// Reads the installed face a request names, if there is one.
pub type Provider = fn(&FaceRequest) -> Option<NamedFace>;

static PROVIDER: OnceLock<Provider> = OnceLock::new();

/// Name the function that finds this machine's installed faces. The first call wins.
pub fn set_named_face_provider(provider: Provider) {
    let _ = PROVIDER.set(provider);
}

/// The installed face `request` names, or `None` when there is none or no provider was named.
pub fn named_face(request: &FaceRequest) -> Option<NamedFace> {
    PROVIDER.get().and_then(|find| find(request))
}

/// What an installed face calls itself, reduced to the keys a request is matched against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceName {
    /// The family of the face's `name` table (ID 1, the one a Windows document names it by) as a
    /// [`key`]: `Arial`, `Arial Narrow` and `Arial Black` are three families.
    pub family_key: String,
    /// Its PostScript name (ID 6) as a [`key`]: `ArialMT`, `Arial-BoldMT`.
    pub ps_key: String,
    pub bold: bool,
    pub italic: bool,
}

/// `s` reduced to lower-case ASCII letters and digits, so `Arial-BoldMT`, `Arial BoldMT` and
/// `arialboldmt` are the same name.
pub fn key(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// What a PDF `/BaseFont` says about the family and style it wants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestedName {
    pub family_key: String,
    pub bold: bool,
    pub italic: bool,
}

/// Style words that follow the family in a PDF font name. Longest first, so `bolditalic` is not
/// read as `bold` and `italic` left over.
const STYLE_WORDS: [&str; 12] =
    ["bolditalic", "boldoblique", "semibold", "demibold", "bold", "italic", "oblique", "regular", "roman", "normal", "psmt", "mt"];

/// Split a `/BaseFont` such as `Arial,Bold`, `Arial-BoldItalicMT`, `ArialNarrow-Bold`, `ArialMT` or
/// `Arial-Black` into its family and style. A word that is not a style (`Black`, `Narrow`,
/// `Light`) stays in the family: those are families of their own on a machine, not weights of
/// `Arial`.
pub fn parse_pdf_name(name: &str) -> RequestedName {
    let (base, tail) = name.split_once([',', '-']).unwrap_or((name, ""));
    let mut family = key(base);
    for suffix in ["psmt", "mt", "ps"] {
        if let Some(stem) = family.strip_suffix(suffix)
            && stem.len() >= 3
        {
            family = stem.to_string();
            break;
        }
    }
    let mut rest = key(tail);
    let (mut bold, mut italic) = (false, false);
    for word in STYLE_WORDS {
        while rest.contains(word) {
            rest = rest.replacen(word, "", 1);
            bold |= word.contains("bold");
            italic |= word.contains("italic") || word.contains("oblique");
        }
    }
    family.push_str(&rest);
    RequestedName { family_key: family, bold, italic }
}

/// The style of a face from its `name` table subfamily (ID 2): `Regular`, `Bold`, `Italic`,
/// `Bold Italic`.
pub fn style_of_subfamily(subfamily: &str) -> (bool, bool) {
    let s = subfamily.to_ascii_lowercase();
    (s.contains("bold"), s.contains("italic") || s.contains("oblique"))
}

/// Families that draw Thai, by the family of their `/BaseFont`: the Windows and macOS Thai text
/// faces, the TLWG faces Linux ships, and the Unicode faces Thai documents are set in. A document
/// that names one of them without embedding it is drawn with a Thai face when nothing named it
/// installed: a Latin stand-in has no Thai at all.
const THAI_FAMILIES: [&str; 32] = [
    "angsana",
    "cordia",
    "browallia",
    "dilleniaupc",
    "eucrosiaupc",
    "freesiaupc",
    "irisupc",
    "jasmineupc",
    "kodchiangupc",
    "lilyupc",
    "thsarabun",
    "sarabun",
    "leelawadee",
    "norasi",
    "garuda",
    "loma",
    "kinnari",
    "waree",
    "purisa",
    "sawasdee",
    "umpush",
    "laksaman",
    "tlwg",
    "thonburi",
    "ayuthaya",
    "krungthep",
    "sathu",
    "silom",
    "tahoma",
    "arialunicodems",
    "microsoftsansserif",
    "notosansthai",
];

/// Whether a `/BaseFont` names a family that draws Thai (see [`THAI_FAMILIES`]).
pub fn is_thai_capable_family(post_script_name: &str) -> bool {
    let family = parse_pdf_name(post_script_name).family_key;
    !family.is_empty() && (THAI_FAMILIES.iter().any(|f| family.starts_with(f)) || family.starts_with("segoeui"))
}

/// The face of `faces` that `request` names, as an index into `faces`: the one whose PostScript
/// name is the requested one, else the one of the requested family and style. `None` when no
/// installed face is that family *and* style: a bold request is never answered with a regular face
/// of the family (the bundled bold stand-in is closer), except for the heavy families (`Arial
/// Black`), whose one face is the heavy one.
pub fn best_match(faces: &[FaceName], request: &FaceRequest) -> Option<usize> {
    let wanted = parse_pdf_name(&request.post_script_name);
    let ps = key(&request.post_script_name);
    if !ps.is_empty()
        && let Some(i) = faces.iter().position(|f| f.ps_key == ps)
    {
        return Some(i);
    }
    let bold = request.bold || request.weight >= 600 || wanted.bold;
    let italic = request.italic || wanted.italic;
    let mut families = vec![wanted.family_key];
    families.extend(request.family.as_deref().map(key));
    families.retain(|f| !f.is_empty());
    families.iter().find_map(|family| {
        let heavy = family.ends_with("black") || family.ends_with("heavy");
        faces
            .iter()
            .position(|f| &f.family_key == family && f.bold == bold && f.italic == italic)
            .or_else(|| heavy.then(|| faces.iter().position(|f| &f.family_key == family && f.italic == italic)).flatten())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(name: &str) -> (String, bool, bool) {
        let p = parse_pdf_name(name);
        (p.family_key, p.bold, p.italic)
    }

    #[test]
    fn pdf_names_split_into_family_and_style() {
        let s = |f: &str, b, i| (f.to_string(), b, i);
        assert_eq!(parsed("ArialMT"), s("arial", false, false));
        assert_eq!(parsed("Arial"), s("arial", false, false));
        assert_eq!(parsed("Arial,Bold"), s("arial", true, false));
        assert_eq!(parsed("Arial-BoldMT"), s("arial", true, false));
        assert_eq!(parsed("Arial-ItalicMT"), s("arial", false, true));
        assert_eq!(parsed("Arial-BoldItalicMT"), s("arial", true, true));
        assert_eq!(parsed("TimesNewRomanPS-BoldMT"), s("timesnewroman", true, false));
        assert_eq!(parsed("TimesNewRoman,Italic"), s("timesnewroman", false, true));
        assert_eq!(parsed("CourierNewPSMT"), s("couriernew", false, false));
        assert_eq!(parsed("Tahoma-Bold"), s("tahoma", true, false));
        assert_eq!(parsed("Calibri-Light"), s("calibrilight", false, false));
        assert_eq!(parsed("THSarabunNew-BoldItalic"), s("thsarabunnew", true, true));
    }

    #[test]
    fn a_word_that_is_not_a_style_stays_in_the_family() {
        // Arial Black and Arial Narrow are families of their own, not weights of Arial.
        assert_eq!(parsed("Arial-Black").0, "arialblack");
        assert_eq!(parsed("ArialNarrow").0, "arialnarrow");
        assert_eq!(parsed("ArialNarrow-Bold"), ("arialnarrow".to_string(), true, false));
        assert_eq!(parsed("ArialUnicodeMS").0, "arialunicodems");
    }

    #[test]
    fn short_names_are_not_cut_down_to_nothing() {
        assert_eq!(parsed("PS").0, "ps");
        assert_eq!(parsed("MT").0, "mt");
        assert_eq!(parsed("").0, "");
        assert_eq!(parsed("-").0, "");
    }

    fn face(family: &str, ps: &str, bold: bool, italic: bool) -> FaceName {
        FaceName { family_key: key(family), ps_key: key(ps), bold, italic }
    }

    fn request(name: &str, bold: bool, weight: u32) -> FaceRequest {
        FaceRequest { post_script_name: name.into(), bold, italic: false, weight, family: None }
    }

    fn installed() -> Vec<FaceName> {
        vec![
            face("Arial", "ArialMT", false, false),
            face("Arial", "Arial-BoldMT", true, false),
            face("Arial", "Arial-ItalicMT", false, true),
            face("Arial Black", "Arial-Black", false, false),
            face("Arial Narrow", "ArialNarrow", false, false),
            face("Tahoma", "Tahoma", false, false),
        ]
    }

    #[test]
    fn the_postscript_name_wins() {
        let faces = installed();
        assert_eq!(best_match(&faces, &request("ArialMT", false, 400)), Some(0));
        assert_eq!(best_match(&faces, &request("Arial-BoldMT", false, 400)), Some(1));
        assert_eq!(best_match(&faces, &request("Arial-Black", false, 400)), Some(3));
    }

    #[test]
    fn family_and_style_find_a_face_the_name_does_not_spell() {
        let faces = installed();
        assert_eq!(best_match(&faces, &request("Arial,Bold", true, 400)), Some(1), "Arial,Bold is the bold face of Arial");
        assert_eq!(best_match(&faces, &request("ArialMT", true, 700)), Some(0), "the PostScript name is checked first");
        assert_eq!(best_match(&faces, &request("Arial,Italic", false, 400)), Some(2));
        assert_eq!(best_match(&faces, &request("Arial", false, 700)), Some(1), "a descriptor weight of 700 is bold");
    }

    #[test]
    fn a_style_the_family_lacks_is_not_made_up() {
        let faces = installed();
        // Tahoma is installed in regular only: Tahoma-Bold must not be answered with Tahoma regular.
        assert_eq!(best_match(&faces, &request("Tahoma-Bold", true, 700)), None);
        // An unknown family finds nothing, so the bundled stand-in is drawn.
        assert_eq!(best_match(&faces, &request("Angsana New", false, 400)), None);
    }

    #[test]
    fn a_heavy_family_is_answered_by_its_one_face() {
        let faces = installed();
        // `Arial-Black` with `/FontWeight 900`: the one Arial Black face is already the heavy one.
        let mut r = request("Arial,Black", true, 900);
        r.post_script_name = "ArialBlack".into();
        assert_eq!(best_match(&faces, &r), Some(3));
    }

    #[test]
    fn the_descriptor_family_is_a_second_chance() {
        let faces = installed();
        let mut r = request("SomeExporterRename", false, 400);
        r.family = Some("Tahoma".into());
        assert_eq!(best_match(&faces, &r), Some(5));
    }

    #[test]
    fn no_faces_and_empty_requests_find_nothing() {
        assert_eq!(best_match(&[], &request("ArialMT", false, 400)), None);
        assert_eq!(best_match(&installed(), &request("", false, 400)), None);
    }

    #[test]
    fn thai_capable_families_are_known_by_name() {
        for name in [
            "AngsanaNew",
            "AngsanaUPC",
            "Angsana New,Bold",
            "CordiaNew",
            "BrowalliaUPC-Bold",
            "DilleniaUPC",
            "THSarabunNew",
            "TH-Sarabun-New",
            "Leelawadee",
            "LeelawadeeUI-Bold",
            "Garuda",
            "Loma",
            "Norasi",
            "Kinnari",
            "Waree",
            "Tahoma",
            "Tahoma-Bold",
            "ArialUnicodeMS",
            "MicrosoftSansSerif",
            "SegoeUI",
            "Thonburi",
            "NotoSansThai-Regular",
        ] {
            assert!(is_thai_capable_family(name), "{name}");
        }
        for name in ["ArialMT", "Arial,Bold", "TimesNewRomanPSMT", "Calibri", "CourierNew", "Helvetica", "ASDNoSuchFamily", "Blackadder", "", "-"] {
            assert!(!is_thai_capable_family(name), "{name}");
        }
    }

    #[test]
    fn without_a_provider_nothing_is_found() {
        // The provider is process-wide and named once by the desktop app, never by this crate's tests.
        assert!(named_face(&request("ArialMT", false, 400)).is_none());
    }
}
