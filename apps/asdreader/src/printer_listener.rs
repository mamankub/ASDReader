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
//! 3. saves it as `Printed yyyy-mm-dd hh.mm.ss.pdf` in the user's `Documents\ASDReader Printed`,
//! 4. opens it in ASDReader (the running one, through the usual single-instance hand-off).
//!
//! The name of the file never comes from the stream, nothing is run through a shell, and a stream
//! that is not a PDF is dropped. It starts with the user's session (a `Run` entry the installer
//! writes) and a second copy exits quietly, as the port is taken.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// Where printed files go; the user's `Documents\ASDReader Printed` when `None`.
    pub out_dir: Option<PathBuf>,
    /// Open each printed file in ASDReader.
    pub open: bool,
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
    let Some(dir) = options.out_dir.or_else(default_dir) else {
        log::error!("printer listener: no folder for printed files (no Documents folder)");
        return 1;
    };
    log::info!("printer listener: waiting on 127.0.0.1:{}, printed files go to {}", options.port, dir.display());
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => match receive(stream) {
                Ok(bytes) => match save(&dir, &bytes) {
                    Ok(path) => {
                        log::info!("printer listener: saved {} ({} bytes)", path.display(), bytes.len());
                        if options.open {
                            open(&path);
                        }
                    }
                    Err(e) => log::error!("printer listener: {e}"),
                },
                Err(e) => log::warn!("printer listener: dropped a job: {e}"),
            },
            Err(e) => log::warn!("printer listener: {e}"),
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

/// `Documents\ASDReader Printed` of the current user.
fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).filter(|p| p.is_absolute())?;
    Some(home.join("Documents").join("ASDReader Printed"))
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
    use asdreader_engine::{Edit, Session};
    let original = Arc::new(bytes);
    let mut session = Session::new();
    let Ok(id) = session.open("printed.pdf", None, Arc::clone(&original), None) else {
        return original.to_vec();
    };
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
    let temp = dir.join(format!(".printing-{}.tmp", std::process::id()));
    let out = repaired(bytes.to_vec());
    let write = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&temp)?;
        f.write_all(&out)?;
        f.sync_all()?;
        std::fs::rename(&temp, &target)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("writing {}: {e}", target.display())
    })?;
    Ok(target)
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
