//! Thai faces installed on this machine, for writing Thai into a paragraph whose own font has no
//! Thai (a document font that was never embedded, or a subset without those glyphs).
//!
//! The bundled Noto Sans Thai is a variable font, which can't be embedded as it is, and loopless,
//! which Thai readers don't take for document text. A desktop build names a provider of installed
//! static faces (Tahoma, Leelawadee, TH Sarabun New…) with [`set_installed_thai_provider`]; it is
//! called once, the first time a paragraph needs one, and only faces that may be embedded are kept.
//! Without a provider (the CLI, the web build, tests) there are none.

use std::sync::{Arc, OnceLock};

use skrifa::raw::TableProvider;
use skrifa::{FontRef, MetadataProvider};

/// One installed face: its family name, whether it is bold, and its program.
#[derive(Clone, Debug)]
pub struct InstalledFace {
    pub family: String,
    pub bold: bool,
    pub bytes: Arc<Vec<u8>>,
}

impl InstalledFace {
    /// The face in `bytes`, if it is a standalone font that may be embedded and has Thai.
    pub fn new(bytes: Vec<u8>) -> Option<Self> {
        crate::embedding::validate(&bytes).ok()?;
        let font = FontRef::new(&bytes).ok()?;
        font.charmap().map('\u{0E01}')?;
        let bold = font.os2().is_ok_and(|os2| os2.us_weight_class() >= 600);
        let (family, _) = crate::embedding::names(&bytes).ok()?;
        Some(Self { family, bold, bytes: Arc::new(bytes) })
    }

    /// Whether the face has a glyph for every character of `text`.
    pub fn covers(&self, text: &str) -> bool {
        crate::embedding::covers(&self.bytes, text)
    }
}

/// Reads the installed Thai faces, best first.
pub type Provider = fn() -> Vec<InstalledFace>;

static PROVIDER: OnceLock<Provider> = OnceLock::new();
static FACES: OnceLock<Vec<InstalledFace>> = OnceLock::new();

/// Name the function that reads this machine's Thai faces. The first call wins.
pub fn set_installed_thai_provider(provider: Provider) {
    let _ = PROVIDER.set(provider);
}

/// The installed Thai faces that may be embedded, best first; read once.
pub fn installed_thai_faces() -> &'static [InstalledFace] {
    FACES.get_or_init(|| PROVIDER.get().map(|read| read()).unwrap_or_default())
}

/// The installed face for `text` (Thai): the first with every character, of the weight asked for
/// when there is one.
pub fn installed_thai_face(text: &str, bold: bool) -> Option<&'static InstalledFace> {
    pick(installed_thai_faces(), text, bold)
}

/// The first of `faces` with every character of `text` and the weight asked for, else the first
/// with every character.
pub fn pick<'a>(faces: &'a [InstalledFace], text: &str, bold: bool) -> Option<&'a InstalledFace> {
    faces.iter().find(|f| f.bold == bold && f.covers(text)).or_else(|| faces.iter().find(|f| f.covers(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(family: &str, bold: bool, bytes: &[u8]) -> InstalledFace {
        InstalledFace { family: family.into(), bold, bytes: Arc::new(bytes.to_vec()) }
    }

    #[test]
    fn the_face_of_the_weight_asked_for_comes_first_among_those_with_the_text() {
        let inter: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        let faces = [face("Regular", false, inter), face("Bold", true, inter), face("Other", false, inter)];
        assert_eq!(pick(&faces, "abc", true).map(|f| f.family.as_str()), Some("Bold"));
        assert_eq!(pick(&faces, "abc", false).map(|f| f.family.as_str()), Some("Regular"));
        assert_eq!(pick(&faces[2..], "abc", true).map(|f| f.family.as_str()), Some("Other"), "any weight before none");
        assert!(pick(&faces, "ก", false).is_none(), "Inter has no Thai");
        assert!(InstalledFace::new(inter.to_vec()).is_none(), "a face without Thai is not taken");
        assert!(InstalledFace::new(b"not a font".to_vec()).is_none());
    }
}
