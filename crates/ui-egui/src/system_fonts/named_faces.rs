//! Installed faces found by the name a PDF gives them (`asdreader_fonts::named`).
//!
//! A PDF that names a font and does not embed it (`/BaseFont /ArialMT`) is drawn with the face of
//! that name installed on this machine when there is one. This is the file side: it lists the
//! machine's font files once, reads only the `name` table of each (two small reads, never the
//! whole font) to learn each face's family, style and PostScript name, and reads a file's bytes
//! only when a document asks for one of its faces. Faces are only drawn with: never embedded,
//! shipped, copied or committed (AGENTS.md §1.4), and `ASDREADER_SYSTEM_FONTS=0` turns them off.
//!
//! The files are untrusted input. Every length, offset and count read from one is bounded and
//! checked, and a file that is not a font, is truncated or lies about itself yields no face.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use asdreader_fonts::named::{FaceName, FaceRequest, NamedFace, best_match, key, style_of_subfamily};

/// Larger font files are not read: a font path is still untrusted input.
const MAX_FILE: u64 = 32 << 20;
/// Faces listed in one collection (`.ttc`).
const MAX_FACES_PER_FILE: u32 = 16;
/// Tables in one face's directory.
const MAX_TABLES: usize = 64;
/// The `name` table is a few KiB; one this large is not a font's.
const MAX_NAME_TABLE: usize = 1 << 20;
/// Name records read from one table.
const MAX_NAME_RECORDS: usize = 512;
/// Font files listed in all, and how deep their folders go.
const MAX_FILES: usize = 6000;
const MAX_DEPTH: usize = 4;
/// Font programs kept in memory at once (a collection is one entry, however many faces it has).
const MAX_CACHED: usize = 48;

/// The installed face a PDF font is named after, or `None` when installed fonts are off or there
/// is no face of that family and style.
pub fn named_face(request: &FaceRequest) -> Option<NamedFace> {
    if std::env::var_os("ASDREADER_SYSTEM_FONTS").is_some_and(|v| v == "0") {
        return None;
    }
    static INDEX: OnceLock<Index> = OnceLock::new();
    let index = INDEX.get_or_init(|| Index::from_files(&candidate_files()));
    let found = index.find(request)?;
    log::info!("installed font for {}: {} (face {})", request.post_script_name, found.0.display(), found.1);
    Some(NamedFace { bytes: bytes_of(&found.0)?, index: found.1 })
}

/// What the machine's font files hold: each face's names, and where it is.
#[derive(Default)]
struct Index {
    names: Vec<FaceName>,
    places: Vec<(PathBuf, u32)>,
}

impl Index {
    fn from_files(files: &[PathBuf]) -> Self {
        let mut index = Self::default();
        for path in files {
            for (face, name) in face_names(path) {
                index.names.push(name);
                index.places.push((path.clone(), face));
            }
        }
        index
    }

    fn find(&self, request: &FaceRequest) -> Option<&(PathBuf, u32)> {
        self.places.get(best_match(&self.names, request)?)
    }
}

/// A font file's bytes, kept for the next tile or page that draws with it.
fn bytes_of(path: &Path) -> Option<Arc<Vec<u8>>> {
    static CACHE: Mutex<Option<HashMap<PathBuf, Arc<Vec<u8>>>>> = Mutex::new(None);
    if let Ok(cache) = CACHE.lock()
        && let Some(bytes) = cache.as_ref().and_then(|c| c.get(path))
    {
        return Some(Arc::clone(bytes));
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    let bytes = Arc::new(std::fs::read(path).ok()?);
    if let Ok(mut cache) = CACHE.lock() {
        let map = cache.get_or_insert_with(HashMap::new);
        if map.len() >= MAX_CACHED {
            map.clear();
        }
        map.insert(path.to_path_buf(), Arc::clone(&bytes));
    }
    Some(bytes)
}

/// Every font file this machine has, in no particular order.
fn candidate_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut budget = MAX_FILES;
    if cfg!(windows) {
        walk(&super::windows_fonts(), 0, &mut files, &mut budget);
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            walk(&PathBuf::from(local).join("Microsoft/Windows/Fonts"), 0, &mut files, &mut budget);
        }
    } else if cfg!(target_os = "macos") {
        for dir in super::MACOS_FONT_DIRS {
            walk(Path::new(dir), 0, &mut files, &mut budget);
        }
        walk(Path::new("/Library/Fonts"), 0, &mut files, &mut budget);
        if let Some(home) = std::env::var_os("HOME") {
            walk(&PathBuf::from(home).join("Library/Fonts"), 0, &mut files, &mut budget);
        }
    } else {
        files.extend(unix_font_files());
        files.truncate(MAX_FILES);
    }
    files
}

#[cfg(all(unix, not(target_os = "macos"), not(target_arch = "wasm32")))]
fn unix_font_files() -> Vec<PathBuf> {
    super::fontconfig::font_files(&super::fontconfig::config_path())
}

#[cfg(not(all(unix, not(target_os = "macos"), not(target_arch = "wasm32"))))]
fn unix_font_files() -> Vec<PathBuf> {
    Vec::new()
}

/// The font files under `dir`, within a depth and a total count. Links are followed, and the depth
/// bound ends a loop of them.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, budget: &mut usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out, budget);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(is_font_extension) {
            out.push(path);
        }
    }
}

fn is_font_extension(ext: &str) -> bool {
    ["ttf", "otf", "ttc", "otc"].iter().any(|e| ext.eq_ignore_ascii_case(e))
}

/// The faces of one font file with what each calls itself; none when it is not a font.
fn face_names(path: &Path) -> Vec<(u32, FaceName)> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let Ok(meta) = file.metadata() else { return Vec::new() };
    face_names_from(&mut file, meta.len())
}

/// [`face_names`] on any reader of `len` bytes.
fn face_names_from<R: Read + Seek>(reader: &mut R, len: u64) -> Vec<(u32, FaceName)> {
    if !(12..=MAX_FILE).contains(&len) {
        return Vec::new();
    }
    let Some(head) = read_at(reader, 0, 12) else { return Vec::new() };
    let offsets: Vec<u64> = if head.get(0..4) == Some(b"ttcf") {
        let count = be32(&head, 8).unwrap_or(0).min(MAX_FACES_PER_FILE);
        let Some(table) = read_at(reader, 12, usize::try_from(count).unwrap_or(0) * 4) else { return Vec::new() };
        (0..count as usize).filter_map(|i| be32(&table, i * 4)).map(u64::from).collect()
    } else if matches!(head.get(0..4), Some(b"\0\x01\0\0" | b"true" | b"OTTO")) {
        vec![0]
    } else {
        return Vec::new();
    };
    offsets.into_iter().enumerate().filter_map(|(i, offset)| Some((u32::try_from(i).ok()?, face_name(reader, len, offset)?))).collect()
}

/// The names of the face whose table directory is at `offset`.
fn face_name<R: Read + Seek>(reader: &mut R, len: u64, offset: u64) -> Option<FaceName> {
    let dir = read_at(reader, offset, 12)?;
    let tables = usize::from(be16(&dir, 4)?);
    if tables == 0 || tables > MAX_TABLES {
        return None;
    }
    let records = read_at(reader, offset.checked_add(12)?, tables * 16)?;
    let (at, size) = (0..tables).find_map(|i| {
        let record = records.get(i * 16..i * 16 + 16)?;
        (record.get(0..4)? == b"name").then(|| Some((u64::from(be32(record, 8)?), usize::try_from(be32(record, 12)?).ok()?)))?
    })?;
    if !(6..=MAX_NAME_TABLE).contains(&size) || at.checked_add(u64::try_from(size).ok()?)? > len {
        return None;
    }
    parse_name_table(&read_at(reader, at, size)?)
}

/// The family (ID 1), subfamily (ID 2) and PostScript name (ID 6) of a `name` table: English
/// records first, then whichever comes first.
fn parse_name_table(table: &[u8]) -> Option<FaceName> {
    let count = usize::from(be16(table, 2)?).min(MAX_NAME_RECORDS);
    let strings = usize::from(be16(table, 4)?);
    // (is English, text) for the family, the subfamily and the PostScript name.
    let mut slots: [Option<(bool, String)>; 3] = [None, None, None];
    for i in 0..count {
        let Some(record) = table.get(6 + i * 12..6 + i * 12 + 12) else { break };
        let (platform, encoding, language, id) = (be16(record, 0)?, be16(record, 2)?, be16(record, 4)?, be16(record, 6)?);
        let (length, offset) = (usize::from(be16(record, 8)?), usize::from(be16(record, 10)?));
        let slot = match id {
            1 => 0,
            2 => 1,
            6 => 2,
            _ => continue,
        };
        let unicode = platform == 3 || platform == 0;
        if !unicode && !(platform == 1 && encoding == 0) {
            continue;
        }
        let start = strings.checked_add(offset)?;
        let Some(raw) = table.get(start..start.checked_add(length)?) else { continue };
        let text = if unicode {
            String::from_utf16_lossy(&raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect::<Vec<u16>>())
        } else {
            raw.iter().map(|&b| char::from(b)).collect()
        };
        let english = (platform == 3 && language == 0x409) || (platform == 1 && language == 0);
        let replace = match &slots[slot] {
            None => true,
            Some((was_english, _)) => !was_english && english,
        };
        if replace {
            slots[slot] = Some((english, text));
        }
    }
    let [family, subfamily, post_script] = slots.map(|s| s.map(|(_, text)| text).unwrap_or_default());
    let family_key = key(&family);
    if family_key.is_empty() {
        return None;
    }
    let (bold, italic) = style_of_subfamily(&subfamily);
    Some(FaceName { family_key, ps_key: key(&post_script), bold, italic })
}

fn read_at<R: Read + Seek>(reader: &mut R, at: u64, len: usize) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut buffer = vec![0; len];
    reader.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const INTER: &[u8] = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf");
    const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf");

    fn names_of(bytes: &[u8]) -> Vec<(u32, FaceName)> {
        face_names_from(&mut Cursor::new(bytes.to_vec()), bytes.len() as u64)
    }

    fn request(name: &str) -> FaceRequest {
        FaceRequest { post_script_name: name.into(), family: None, bold: false, italic: false, weight: 400 }
    }

    #[test]
    fn a_font_file_reports_the_names_it_gives_itself() {
        let faces = names_of(INTER);
        assert_eq!(faces.len(), 1);
        let (index, name) = &faces[0];
        assert_eq!(*index, 0);
        assert_eq!(name.family_key, "inter");
        assert_eq!(name.ps_key, "interregular");
        assert!(!name.bold && !name.italic);
    }

    /// A scratch folder of real font files, removed when it goes out of scope.
    struct Folder(PathBuf);

    impl Folder {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("asdreader-named-faces-{tag}-{}-{nanos}", std::process::id()));
            assert!(std::fs::create_dir_all(dir.join("nested")).is_ok());
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_machines_fonts_are_listed_and_found_by_name_without_reading_whole_files() {
        let folder = Folder::new("index");
        assert!(std::fs::write(folder.0.join("Inter-Regular.ttf"), INTER).is_ok());
        assert!(std::fs::write(folder.0.join("nested/semibold.TTF"), INTER_SEMIBOLD).is_ok());
        assert!(std::fs::write(folder.0.join("notes.txt"), b"not a font").is_ok());
        assert!(std::fs::write(folder.0.join("broken.ttf"), b"not a font either, only named like one").is_ok());

        let (mut files, mut budget) = (Vec::new(), 100);
        walk(&folder.0, 0, &mut files, &mut budget);
        assert_eq!(files.len(), 3, "the .txt is not a font file: {files:?}");

        let index = Index::from_files(&files);
        assert_eq!(index.names.len(), 2, "the file that is not a font adds no face");
        let regular = index.find(&request("Inter-Regular")).map(|p| p.0.file_name().map(|n| n.to_owned()));
        assert_eq!(regular, Some(Some("Inter-Regular.ttf".into())));
        assert!(index.find(&request("Inter")).is_some(), "the family alone finds its regular face");
        assert!(index.find(&request("NotInstalledAnywhere")).is_none());
        let mut bold = request("Inter,Bold");
        bold.bold = true;
        assert!(index.find(&bold).is_none(), "no installed Inter is bold: a bold request is not answered with a regular face");
        // Inter SemiBold is a family of its own in the name table a Windows document names it by.
        let semibold = index.find(&request("Inter-SemiBold")).map(|p| p.0.file_name().map(|n| n.to_owned()));
        assert_eq!(semibold, Some(Some("semibold.TTF".into())));
        assert!(index.find(&request("InterSemiBold")).is_some(), "a name with no separator finds the family of that name");
    }

    #[test]
    fn a_face_found_by_name_is_read_whole_only_then_and_reused() {
        let folder = Folder::new("bytes");
        let path = folder.0.join("Inter-Regular.ttf");
        assert!(std::fs::write(&path, INTER).is_ok());
        let first = bytes_of(&path);
        let second = bytes_of(&path);
        assert_eq!(first.as_ref().map(|b| b.len()), Some(INTER.len()));
        assert!(matches!((&first, &second), (Some(a), Some(b)) if Arc::ptr_eq(a, b)), "the second read is the cached program");
        assert!(bytes_of(&folder.0).is_none(), "a folder is not a font");
        assert!(bytes_of(&folder.0.join("missing.ttf")).is_none());
    }

    #[test]
    fn a_file_that_lies_about_itself_yields_no_face_and_no_panic() {
        // Every prefix of a real font: the table directory, then the name table, cut short.
        for cut in (0..INTER.len().min(4096)).step_by(7) {
            let _ = names_of(INTER.get(..cut).unwrap_or_default());
        }
        // A collection header that claims four billion faces.
        let mut ttc = b"ttcf\0\x01\0\0\xff\xff\xff\xff".to_vec();
        ttc.extend_from_slice(&[0; 64]);
        assert!(names_of(&ttc).is_empty());
        // A table directory whose `name` table points far past the end of the file.
        let mut bad = INTER.get(..12 + 16 * 40).unwrap_or_default().to_vec();
        for chunk in bad.get_mut(12..).unwrap_or_default().chunks_mut(16) {
            if chunk.get(0..4) == Some(b"name") {
                chunk.get_mut(8..12).unwrap_or_default().copy_from_slice(&[0xff, 0xff, 0xff, 0xf0]);
            }
        }
        assert!(names_of(&bad).is_empty());
        // Noise, empty input and a header that names no tables.
        assert!(names_of(&[]).is_empty());
        assert!(names_of(&[0x37; 5000]).is_empty());
        assert!(names_of(b"OTTO\0\0\0\0\0\0\0\0").is_empty());
    }

    #[test]
    fn a_name_table_with_records_off_its_end_is_read_as_far_as_it_goes() {
        // Header: format 0, 3 records, strings at offset 6 + 3 * 12 = 42 (records cut after the first).
        let mut table = vec![0, 0, 0, 3, 0, 42];
        table.extend_from_slice(&[0, 3, 0, 1, 0x04, 0x09, 0, 1, 0, 6, 0, 0]); // (3,1,0x409) family, 3 bytes at 0
        table.extend_from_slice(&[0xff; 7]); // a record cut off by the end of the table
        assert!(parse_name_table(&table).is_none(), "no usable family: no face");
        assert!(parse_name_table(&[]).is_none());
        assert!(parse_name_table(&[0, 0, 0xff, 0xff, 0xff, 0xff]).is_none());
    }
}
