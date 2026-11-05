//! The receiving end of the "ASD PDF Printer" print queue (`ASDReader --printer-listener`).
//!
//! The queue (packaging/windows/asd-pdf-printer.ps1) is Windows' own "Microsoft Print To PDF"
//! driver on a TCP port at 127.0.0.1:[`PORT`]. The driver turns what any program prints into a PDF
//! and the spooler sends those bytes here. This process
//!
//! 1. reads the PDF (at most [`MAX_JOB`] bytes, only from this computer),
//! 2. repairs its Thai text layer ([`asdreader_engine::Edit::RepairThaiText`]): the driver leaves
//!    the lowered and shifted tone marks out of `/ToUnicode`, so without this the printed file
//!    can't be searched, copied or edited as Thai in any viewer,
//! 3. asks where to save it, with a Save dialog brought in front of the program that printed
//!    (as Microsoft Print to PDF does): the last folder chosen, and the document's title (else
//!    `Printed yyyy-mm-dd hh.mm.ss`) as the name. Cancel drops the job,
//! 4. opens it in ASDReader (the running one, through the usual single-instance hand-off).
//!
//! With `--out-dir` (tests and unattended set-ups) nothing is asked: each job is saved there as
//! `Printed yyyy-mm-dd hh.mm.ss.pdf`.
//!
//! Without the dialog the name of the file never comes from the stream; nothing is run through a
//! shell, and a stream that is not a PDF is dropped. It starts with the user's session (a `Run` entry the installer
//! writes) and a second copy exits quietly, as the port is taken.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// The port of the ASD PDF Printer's queue.
pub const PORT: u16 = 19100;
/// Largest print job taken.
const MAX_JOB: u64 = 1 << 30;
/// How long a sender may stay silent before its job is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// What the command line asked for.
pub struct Options {
    pub port: u16,
    /// Where printed files go without asking; when `None`, a Save dialog asks for each job.
    pub out_dir: Option<PathBuf>,
    /// Open each printed file in ASDReader.
    pub open: bool,
    /// ASDReader's data folder, where the last folder chosen in the Save dialog is kept.
    pub state_dir: Option<PathBuf>,
}

/// Run until the process is stopped; the exit code.
pub fn run(options: Options) -> i32 {
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, options.port))) {
        Ok(l) => l,
        Err(e) => {
            // Another copy of this listener (or another program) has the port.
            log::info!("printer listener: port {} is taken ({e}); leaving it to the one that has it", options.port);
            return 0;
        }
    };
    match &options.out_dir {
        Some(dir) => log::info!("printer listener: waiting on 127.0.0.1:{}, printed files go to {}", options.port, dir.display()),
        None => log::info!("printer listener: waiting on 127.0.0.1:{}, asking where each printed file goes", options.port),
    }
    for stream in listener.incoming() {
        let bytes = match stream.map_err(|e| e.to_string()).and_then(receive) {
            Ok(bytes) => bytes,
            Err(e) => {
                log::warn!("printer listener: dropped a job: {e}");
                continue;
            }
        };
        let size = bytes.len();
        let saved = match &options.out_dir {
            Some(dir) => save(dir, &bytes).map(Some),
            None => save_as(options.state_dir.as_deref(), bytes),
        };
        match saved {
            Ok(Some(path)) => {
                log::info!("printer listener: saved {} ({size} bytes)", path.display());
                if options.open {
                    open(&path);
                }
            }
            Ok(None) => log::info!("printer listener: the Save dialog was cancelled; the job ({size} bytes) is dropped"),
            Err(e) => log::error!("printer listener: {e}"),
        }
    }
    0
}

/// Start the receiver in the background when this install has the ASD PDF Printer (the queue script
/// sits beside the program) and nothing listens on the port yet, so printing works right after the
/// installer without signing out and in. The `Run` entry does the same at sign-in. A no-op elsewhere.
pub fn ensure_running() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW | DETACHED_PROCESS: the receiver has no console and outlives this window.
        const FLAGS: u32 = 0x0800_0000 | 0x0000_0008;
        let Ok(exe) = std::env::current_exe() else { return };
        if !exe.parent().is_some_and(|dir| dir.join("asd-pdf-printer.ps1").is_file()) {
            return;
        }
        // Bound by someone: a receiver is running.
        if TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, PORT))).is_err() {
            return;
        }
        if let Err(e) = std::process::Command::new(exe).arg("--printer-listener").creation_flags(FLAGS).spawn() {
            log::warn!("printer listener: couldn't start it: {e}");
        }
    }
}

/// The current user's `Documents` folder.
fn documents_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).filter(|p| p.is_absolute())?;
    Some(home.join("Documents"))
}

/// The file in ASDReader's data folder that keeps the last folder chosen for a printed file.
const LAST_FOLDER_FILE: &str = "printer-folder.txt";

/// The folder the Save dialog opens in: the last one chosen, while it still exists, else
/// `Documents`.
fn start_folder(state_dir: Option<&Path>) -> Option<PathBuf> {
    let remembered = state_dir
        .and_then(|dir| std::fs::read_to_string(dir.join(LAST_FOLDER_FILE)).ok())
        .map(|text| PathBuf::from(text.trim()))
        .filter(|dir| dir.is_absolute() && dir.is_dir());
    remembered.or_else(|| documents_dir().filter(|dir| dir.is_dir()))
}

/// Keep the folder of `chosen` for the next Save dialog.
fn remember_folder(state_dir: Option<&Path>, chosen: &Path) {
    let (Some(state_dir), Some(folder)) = (state_dir, chosen.parent()) else { return };
    let kept =
        std::fs::create_dir_all(state_dir).and_then(|()| std::fs::write(state_dir.join(LAST_FOLDER_FILE), folder.to_string_lossy().as_bytes()));
    if let Err(e) = kept {
        log::warn!("printer listener: couldn't keep the folder chosen: {e}");
    }
}

/// The name the Save dialog suggests: the document's title made safe for a file name, else
/// `Printed yyyy-mm-dd hh.mm.ss`.
fn suggested_name(title: Option<&str>) -> String {
    let title: String = title
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Windows drops a trailing dot or space from a name; a long title is cut on a character.
    let title: String = title.trim_end_matches(['.', ' ']).chars().take(120).collect();
    let title = title.trim_end_matches(['.', ' ']);
    let stem = if title.is_empty() { format!("Printed {}", chrono::Local::now().format("%Y-%m-%d %H.%M.%S")) } else { title.to_string() };
    format!("{stem}.pdf")
}

/// `path`, ending in `.pdf` when the name typed has no extension of that kind.
fn with_pdf_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        path
    } else {
        let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".pdf");
        path.with_file_name(name)
    }
}

/// Ask where to save the job and save it there; `None` when the dialog was cancelled.
///
/// The Thai repair runs while the dialog is open. The document's title, for the suggested name,
/// comes from the same reading of the file, waited for a moment only.
fn save_as(state_dir: Option<&Path>, bytes: Vec<u8>) -> Result<Option<PathBuf>, String> {
    let original = Arc::new(bytes);
    let (title_tx, title_rx) = mpsc::channel();
    let worker = {
        let original = Arc::clone(&original);
        std::thread::Builder::new()
            .name("asdreader-printed-repair".into())
            .spawn(move || repaired_with_title(original, |title| drop(title_tx.send(title))))
            .ok()
    };
    let title = title_rx.recv_timeout(Duration::from_secs(2)).ok().flatten();
    let name = suggested_name(title.as_deref());
    let Some(chosen) = ask_where(start_folder(state_dir), &name) else {
        return Ok(None);
    };
    let chosen = with_pdf_extension(chosen);
    remember_folder(state_dir, &chosen);
    let out = worker.and_then(|w| w.join().ok()).unwrap_or_else(|| original.to_vec());
    write_atomically(&chosen, &out)?;
    Ok(Some(chosen))
}

/// The Save dialog, in front of the program that printed; the path chosen.
fn ask_where(folder: Option<PathBuf>, name: &str) -> Option<PathBuf> {
    use asdreader_ui_egui::i18n;
    let lang = i18n::system_lang();
    let title = i18n::tr(lang, "Save the printed PDF");
    let mut dialog = rfd::FileDialog::new().set_title(title).add_filter("PDF", &["pdf"]).set_file_name(name);
    if let Some(folder) = folder {
        dialog = dialog.set_directory(folder);
    }
    #[cfg(windows)]
    let front = to_front::Raise::start(title);
    let chosen = dialog.save_file();
    #[cfg(windows)]
    front.stop();
    chosen
}

/// Bringing the Save dialog in front of the program that printed.
///
/// The receiver runs in the background, and Windows doesn't let a background process take the
/// foreground: its dialog would open behind the program that printed, with only a flashing
/// taskbar button. While the dialog is open, a helper thread finds it by its title and joins the
/// input of the foreground window's thread for a moment, which lets it make the dialog the active
/// window. Whatever happens, the dialog is also put on top of other windows (topmost) while it
/// is open, so it is in sight even when Windows refuses the focus.
#[cfg(windows)]
mod to_front {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use winsafe::{self as w, co};

    /// How long the helper looks for the dialog.
    const LOOK_FOR: Duration = Duration::from_secs(10);

    pub(super) struct Raise {
        done: Arc<AtomicBool>,
        helper: Option<std::thread::JoinHandle<()>>,
    }

    impl Raise {
        /// Start looking for the dialog with `title` (it is shown by the calling thread).
        pub(super) fn start(title: &str) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let title = title.to_string();
            let helper = {
                let done = Arc::clone(&done);
                std::thread::Builder::new()
                    .name("asdreader-printed-dialog".into())
                    .spawn(move || {
                        let start = Instant::now();
                        while !done.load(Ordering::Relaxed) && start.elapsed() < LOOK_FOR {
                            // "#32770" is the class of Windows' dialog boxes, the Save dialog's too.
                            if let Ok(Some(dialog)) = w::HWND::FindWindow(Some(w::AtomStr::from_str("#32770")), Some(&title)) {
                                bring_forward(&dialog);
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(30));
                        }
                    })
                    .ok()
            };
            Self { done, helper }
        }

        /// The dialog has closed.
        pub(super) fn stop(mut self) {
            self.done.store(true, Ordering::Relaxed);
            if let Some(helper) = self.helper.take() {
                let _ = helper.join();
            }
        }
    }

    /// Put `dialog` on top and make it the active window.
    fn bring_forward(dialog: &w::HWND) {
        let keep = co::SWP::NOMOVE | co::SWP::NOSIZE | co::SWP::SHOWWINDOW;
        let _ = dialog.SetWindowPos(w::HwndPlace::Place(co::HWND_PLACE::TOPMOST), w::POINT::default(), w::SIZE::default(), keep);
        let (dialog_thread, _) = dialog.GetWindowThreadProcessId();
        let foreground_thread = w::HWND::GetForegroundWindow().map(|fg| fg.GetWindowThreadProcessId().0).filter(|&t| t != 0 && t != dialog_thread);
        let attached = foreground_thread.is_some_and(|fg| w::AttachThreadInput(fg, dialog_thread, true).is_ok());
        let _ = dialog.BringWindowToTop();
        let raised = dialog.SetForegroundWindow();
        if let (true, Some(fg)) = (attached, foreground_thread) {
            let _ = w::AttachThreadInput(fg, dialog_thread, false);
        }
        if !raised {
            log::info!("printer listener: Windows kept the focus from the Save dialog; it is shown on top");
        }
    }
}

/// The bytes of one print job, when they are a PDF.
fn receive(mut stream: TcpStream) -> Result<Vec<u8>, String> {
    stream.set_read_timeout(Some(READ_TIMEOUT)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    Read::take(&mut stream, MAX_JOB + 1).read_to_end(&mut bytes).map_err(|e| format!("reading the job: {e}"))?;
    if bytes.len() as u64 > MAX_JOB {
        return Err(format!("larger than {} MiB", MAX_JOB >> 20));
    }
    if !is_pdf(&bytes) {
        return Err("not a PDF".into());
    }
    Ok(bytes)
}

/// Whether `bytes` start a PDF: the header is within the first 1024 bytes (ISO 32000-2, 7.5.2).
fn is_pdf(bytes: &[u8]) -> bool {
    bytes.get(..1024.min(bytes.len())).is_some_and(|head| head.windows(5).any(|w| w == b"%PDF-"))
}

/// The PDF with its Thai text layer repaired (the bytes unchanged when there is nothing to repair).
fn repaired(bytes: Vec<u8>) -> Vec<u8> {
    repaired_with_title(Arc::new(bytes), |_| {})
}

/// [`repaired`], handing `title` the document's title (or `None`) as soon as the file is read.
fn repaired_with_title(original: Arc<Vec<u8>>, title: impl FnOnce(Option<String>)) -> Vec<u8> {
    use asdreader_engine::{Edit, Session};
    let mut session = Session::new();
    let Ok(id) = session.open("printed.pdf", None, Arc::clone(&original), None) else {
        title(None);
        return original.to_vec();
    };
    title(session.get(id).and_then(|d| d.info_value("Title")).filter(|t| !t.trim().is_empty()));
    if session.apply(id, Edit::RepairThaiText).is_err() || !session.get(id).is_some_and(|d| d.dirty) {
        return original.to_vec();
    }
    // As Save does: a rewritten file must reopen with its pages before it is used.
    match session.save_full_bytes(id) {
        Ok(out) if session.check_save(id, &out).is_ok() => out.to_vec(),
        _ => original.to_vec(),
    }
}

/// Save the job in `dir` under a name made here; the path.
fn save(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S%.3f");
    let target = dir.join(format!("Printed {stamp}.pdf"));
    write_atomically(&target, &repaired(bytes.to_vec()))?;
    Ok(target)
}

/// Write `bytes` to `target` through a temporary file beside it, replacing a file of that name
/// (the Save dialog has already asked about that).
fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = target.parent().ok_or_else(|| format!("no folder for {}", target.display()))?;
    let temp = dir.join(format!(".printing-{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&temp, target)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("writing {}: {e}", target.display())
    })
}

/// Open `path` in ASDReader: this program again, which hands the file to the running window.
fn open(path: &Path) {
    let Ok(exe) = std::env::current_exe() else { return };
    if let Err(e) = std::process::Command::new(exe).arg(path).spawn() {
        log::warn!("printer listener: couldn't open {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    /// A valid one-page PDF, with its cross-reference table computed.
    fn minimal_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>",
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn only_a_pdf_is_taken() {
        assert!(is_pdf(b"%PDF-1.7\n..."));
        assert!(is_pdf(b"  \r\n%PDF-1.4"), "the header may follow a little leading junk");
        assert!(!is_pdf(b""));
        assert!(!is_pdf(b"GET / HTTP/1.1\r\n\r\n"));
        let mut late = vec![b' '; 2000];
        late.extend_from_slice(b"%PDF-1.7");
        assert!(!is_pdf(&late), "a header past the first 1024 bytes is not one");
    }

    #[test]
    fn a_job_sent_to_the_port_is_saved_under_a_name_made_here() {
        let dir = std::env::temp_dir().join(format!("asdreader-listener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let pdf = minimal_pdf();
        let sent = pdf.clone();
        let sender = std::thread::spawn(move || {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            s.write_all(&sent).unwrap();
        });
        let (stream, _) = listener.accept().unwrap();
        let bytes = receive(stream).unwrap();
        sender.join().unwrap();
        assert_eq!(bytes, pdf);
        let path = save(&dir, &bytes).unwrap();
        assert!(path.starts_with(&dir) && path.file_name().unwrap().to_string_lossy().starts_with("Printed "));
        assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF-"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_suggested_name_is_the_title_made_safe_for_a_file_name() {
        assert_eq!(suggested_name(Some("Report 2026")), "Report 2026.pdf");
        assert_eq!(suggested_name(Some("รายงาน: ไตรมาส 3/2026?")), "รายงาน ไตรมาส 3 2026.pdf");
        assert_eq!(suggested_name(Some("  a\tb\r\nc . ")), "a b c.pdf", "control characters, trailing dots and spaces go");
        assert_eq!(suggested_name(Some(&"ก".repeat(300))).chars().count(), 124, "a long title is cut on a character");
        for none in [None, Some(""), Some(" ... "), Some("???")] {
            let name = suggested_name(none);
            assert!(name.starts_with("Printed ") && name.ends_with(".pdf"), "{none:?} -> {name}");
        }
    }

    #[test]
    fn a_name_typed_without_pdf_gets_it() {
        assert_eq!(with_pdf_extension(PathBuf::from("/x/report")), PathBuf::from("/x/report.pdf"));
        assert_eq!(with_pdf_extension(PathBuf::from("/x/report.PDF")), PathBuf::from("/x/report.PDF"));
        assert_eq!(with_pdf_extension(PathBuf::from("/x/v1.2")), PathBuf::from("/x/v1.2.pdf"));
    }

    #[test]
    fn the_dialog_opens_in_the_last_folder_chosen_while_it_exists() {
        let root = std::env::temp_dir().join(format!("asdreader-listener-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let state = root.join("state");
        let chosen = root.join("chosen");
        std::fs::create_dir_all(&chosen).unwrap();
        assert_ne!(start_folder(Some(&state)), Some(chosen.clone()), "nothing kept yet");
        remember_folder(Some(&state), &chosen.join("report.pdf"));
        assert_eq!(start_folder(Some(&state)), Some(chosen.clone()));
        std::fs::remove_dir_all(&chosen).unwrap();
        assert_ne!(start_folder(Some(&state)), Some(chosen), "a folder that is gone is not offered");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_job_is_written_over_a_file_of_the_name_chosen() {
        let dir = std::env::temp_dir().join(format!("asdreader-listener-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("report.pdf");
        std::fs::write(&target, b"old").unwrap();
        write_atomically(&target, b"%PDF-new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"%PDF-new");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_title_is_handed_over_before_the_repair() {
        let mut title = None;
        let out = repaired_with_title(Arc::new(minimal_pdf()), |t| title = Some(t));
        assert_eq!(title, Some(None), "a file without a title hands over None");
        assert!(out.starts_with(b"%PDF-"));
        let mut title = None;
        let out = repaired_with_title(Arc::new(b"%PDF-1.4 broken".to_vec()), |t| title = Some(t));
        assert_eq!(title, Some(None));
        assert_eq!(out, b"%PDF-1.4 broken", "a file that can't be read is kept as it came");
    }

    #[test]
    fn something_that_is_not_a_pdf_is_dropped() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let sender = std::thread::spawn(move || {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            s.write_all(b"not a pdf at all").unwrap();
        });
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(receive(stream), Err("not a PDF".to_string()));
        sender.join().unwrap();
    }
}
