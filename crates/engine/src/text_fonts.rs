//! Explicit document-font discovery: the fonts installed on this computer that the user may pick
//! in Edit. Interface fallbacks never enter this catalogue.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    pub path: PathBuf,
    /// The face within a collection (`.ttc`); 0 for a single-face file.
    pub index: u32,
    pub family: String,
    pub face: String,
}

/// Largest font file read: a collection holds several faces, so it may be bigger than one face.
const MAX_FILE_BYTES: u64 = 32 << 20;

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
        } else if meta.is_file() && meta.len() <= MAX_FILE_BYTES && font_extension(&path) {
            if meta.len() > budget {
                continue;
            }
            budget = budget.saturating_sub(meta.len());
            let Ok(bytes) = read_capped(&path) else { continue };
            for (index, program) in programs(&bytes) {
                if asdreader_fonts::embedding::validate_document_input(&program).is_err() {
                    continue;
                }
                if let Ok((family, face)) = asdreader_fonts::embedding::names(&program) {
                    files.push(FontFile { path: path.clone(), index, family, face });
                }
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
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "otf", "ttc"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(std::io::Error::other("the font file is larger than 32 MiB"));
    }
    Ok(bytes)
}

/// The standalone font programs in a font file: the file itself, or each face of a collection.
fn programs(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
    if bytes.get(..4) == Some(b"ttcf".as_slice()) {
        (0..asdreader_fonts::embedding::MAX_COLLECTION_FACES)
            .map_while(|i| asdreader_fonts::embedding::collection_face(bytes, i).map(|p| (i, p)))
            .collect()
    } else {
        vec![(0, bytes.to_vec())]
    }
}

impl FontFile {
    /// The font program to embed: the file, or its face out of a collection.
    pub fn program(&self) -> std::io::Result<Vec<u8>> {
        let bytes = read_capped(&self.path)?;
        if bytes.get(..4) == Some(b"ttcf".as_slice()) {
            return asdreader_fonts::embedding::collection_face(&bytes, self.index)
                .ok_or_else(|| std::io::Error::other("the font collection has no such face"));
        }
        Ok(bytes)
    }
}

/// The installed face a PDF's embedded font was cut from: the same PostScript name, else the same
/// family when both are its plain face. `family` and `postscript` are the embedded program's own
/// names ([`asdreader_fonts::embedding::program_names`]), not the PDF's `/BaseFont`, which
/// producers such as PDFsharp replace with `CIDFont+F1`.
pub fn matching_installed<'a>(files: &'a [FontFile], family: &str, postscript: &str) -> Option<&'a FontFile> {
    use asdreader_fonts::embedding::face_key;
    let (family, postscript) = (face_key(family), face_key(postscript));
    if postscript.is_empty() {
        return None;
    }
    files.iter().find(|f| face_key(&f.face) == postscript).or_else(|| {
        // "THSarabunPSK" (plain) against an installed "TH SarabunPSK" whose PostScript name differs.
        (postscript == family).then(|| files.iter().find(|f| face_key(&f.family) == family && f.label() == f.family)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(family: &str, face: &str) -> FontFile {
        FontFile { path: PathBuf::from("x.ttf"), index: 0, family: family.into(), face: face.into() }
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

    #[test]
    fn an_embedded_subset_finds_the_installed_face_it_was_cut_from() {
        let files = [
            file("TH SarabunPSK", "THSarabunPSK-Bold"),
            file("TH SarabunPSK", "THSarabunPSK"),
            file("CordiaUPC", "CordiaUPC"),
            file("Cordia New", "CordiaNew-Bold"),
        ];
        let found = |family, ps| matching_installed(&files, family, ps).map(|f| f.face.as_str());
        assert_eq!(found("TH SarabunPSK", "THSarabunPSK"), Some("THSarabunPSK"));
        assert_eq!(found("TH SarabunPSK", "THSarabunPSK-Bold"), Some("THSarabunPSK-Bold"));
        assert_eq!(found("Cordia New", "CordiaNew-Bold"), Some("CordiaNew-Bold"));
        assert_eq!(found("cordiaupc", "CORDIAUPC"), Some("CordiaUPC"), "names compare loosely");
        // A bold subset never takes the regular face, and an unknown face finds nothing.
        assert_eq!(found("Cordia New", "CordiaNew-Italic"), None);
        assert_eq!(found("Arial Black", "Arial-Black"), None);
        assert_eq!(found("", ""), None);
    }
}
