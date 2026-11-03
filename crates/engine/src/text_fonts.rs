//! Explicit document-font discovery: the fonts installed on this computer that the user may pick
//! in Edit. Interface fallbacks never enter this catalogue.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct FontFile {
    pub path: PathBuf,
    pub family: String,
    pub face: String,
}

/// Standard user-installed font folders; discovery is performed only on request.
pub fn installed_directories() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        let mut dirs = vec![PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts")];
        if let Some(home) = home {
            dirs.push(home.join("Library/Fonts"));
        }
        dirs
    } else if cfg!(windows) {
        let mut dirs = std::env::var_os("WINDIR")
            .or_else(|| std::env::var_os("SystemRoot"))
            .map(|p| PathBuf::from(p).join("Fonts"))
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
        dirs
    } else {
        let mut dirs = vec![PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")];
        if let Some(home) = home {
            dirs.extend([home.join(".local/share/fonts"), home.join(".fonts")]);
        }
        dirs
    }
}

/// Most files looked at in one scan. A Windows installation has well over a thousand font files.
const MAX_ENTRIES: usize = 16_384;
/// Most bytes read in one scan; each file is read, checked and dropped, so memory stays at one
/// file (16 MiB at most) while the scan walks a whole `C:\Windows\Fonts`.
const MAX_SCAN_BYTES: u64 = 4 << 30;

/// Bounded, symlink-free scan. Standalone TrueType/OpenType faces whose own embedding permissions
/// allow it are listed, whatever their licence (see
/// [`asdreader_fonts::embedding::validate_document_input`]): the user chose them on their own
/// computer. The caller controls roots (automation resolves confinement before calling this).
pub fn discover(roots: &[PathBuf]) -> Vec<FontFile> {
    use std::io::Read;
    let mut pending: Vec<_> = roots.iter().take(16).map(|r| (r.clone(), 0usize)).collect();
    let mut files = Vec::new();
    let mut entries = 0usize;
    let mut budget = MAX_SCAN_BYTES;
    let mut seen = std::collections::HashSet::new();
    while let Some((path, depth)) = pending.pop() {
        if entries >= MAX_ENTRIES || budget == 0 {
            break;
        }
        entries += 1;
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.file_type().is_symlink() || !seen.insert(path.clone()) {
            continue;
        }
        if meta.is_dir() && depth < 4 {
            if let Ok(children) = std::fs::read_dir(&path) {
                let remaining = MAX_ENTRIES.saturating_sub(entries.saturating_add(pending.len()));
                pending.extend(children.take(remaining).filter_map(Result::ok).map(|p| (p.path(), depth + 1)));
            }
        } else if meta.is_file() && meta.len() <= (16 << 20) && font_extension(&path) {
            if meta.len() > budget {
                continue;
            }
            budget = budget.saturating_sub(meta.len());
            let mut bytes = Vec::new();
            let read = std::fs::File::open(&path).and_then(|f| f.take((16 << 20) + 1).read_to_end(&mut bytes));
            if read.is_err() || asdreader_fonts::embedding::validate_document_input(&bytes).is_err() {
                continue;
            }
            if let Ok((family, face)) = asdreader_fonts::embedding::names(&bytes) {
                files.push(FontFile { path, family, face });
            }
        }
    }
    files.sort_by_cached_key(|f| (f.family.to_lowercase(), f.face.to_lowercase(), f.path.clone()));
    files
}

impl FontFile {
    /// The name a font list shows: the family, then the style when the face is not the plain one
    /// (`Tahoma`, `Tahoma Bold`). The PostScript name is `Family-Style`, so the style is what follows
    /// the family in it; a face named some other way is shown by its own name.
    pub fn label(&self) -> String {
        let compact: String = self.family.chars().filter(|c| !c.is_whitespace()).collect();
        match self.face.strip_prefix(compact.as_str()).map(|rest| rest.trim_start_matches(['-', '_', ' '])) {
            Some("" | "Regular" | "Roman" | "Book" | "Normal") => self.family.clone(),
            Some(style) => format!("{} {style}", self.family),
            None => self.face.clone(),
        }
    }
}
fn font_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(family: &str, face: &str) -> FontFile {
        FontFile { path: PathBuf::from("x.ttf"), family: family.into(), face: face.into() }
    }

    #[test]
    fn a_list_label_is_the_family_and_its_style() {
        assert_eq!(file("Tahoma", "Tahoma").label(), "Tahoma");
        assert_eq!(file("Tahoma", "Tahoma-Bold").label(), "Tahoma Bold");
        assert_eq!(file("TH Sarabun New", "THSarabunNew-BoldItalic").label(), "TH Sarabun New BoldItalic");
        assert_eq!(file("Noto Sans", "NotoSans-Regular").label(), "Noto Sans");
        // A PostScript name that doesn't follow Family-Style is shown as it is.
        assert_eq!(file("Cordia New", "CordiaNewBold").label(), "Cordia New Bold");
        assert_eq!(file("Angsana New", "Angsana-X").label(), "Angsana-X");
    }

    /// Any installed face the font itself lets an editor embed is listed, licence text or not, and
    /// nothing that is not a usable font is.
    #[test]
    fn discovery_lists_every_embeddable_font_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("asdreader-fonts-{}", std::process::id()));
        let nested = dir.join("truetype/inter");
        std::fs::create_dir_all(&nested).unwrap();
        let inter = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
        std::fs::write(nested.join("Inter-Regular.ttf"), inter).unwrap();
        std::fs::write(dir.join("Inter-Copy.OTF"), inter).unwrap();
        std::fs::write(dir.join("broken.ttf"), b"not a font").unwrap();
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        let found = discover(std::slice::from_ref(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|f| f.family == "Inter" && f.label() == "Inter"));
    }
}
