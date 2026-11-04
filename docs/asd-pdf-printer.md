# ASD PDF Printer

A print queue named **ASD PDF Printer** that the Windows installer adds beside ASDReader. A program
prints to it like to any printer; a Save dialog asks where the PDF goes, and the PDF, with its Thai
text intact, opens in ASDReader.

## How it works

```
any program ──print──▶ "ASD PDF Printer" queue ──PDF over TCP──▶ ASDReader --printer-listener
   (Windows spooler,                                     127.0.0.1:19100        │
    "Microsoft Print To PDF" driver)                                             ▼
                                     repair Thai text layer, ask where to save (Save dialog), open
```

- **The queue** is made by `packaging/windows/asd-pdf-printer.ps1`: a TCP/IP port `ASDPDF:` at
  `127.0.0.1:19100` and a printer on Windows' own *Microsoft Print To PDF* driver. Nothing is
  bundled, no driver is installed or signed, and the script needs Windows 10 or later.
- **The receiver** is `asdreader.exe --printer-listener` (`apps/asdreader/src/printer_listener.rs`).
  It has no window, starts at sign-in (a `Run` entry the installer writes) and whenever ASDReader is
  opened, and exits quietly when another copy has the port. It takes a PDF from this computer only
  (at most 1 GiB, header checked) and hands the saved file to the running ASDReader.
- **The Save dialog** opens for each job, in front of the program that printed (as Microsoft Print
  to PDF's does): it starts in the last folder chosen (kept in ASDReader's data folder as
  `printer-folder.txt`; `Documents` the first time) with the document's title, else
  `Printed yyyy-mm-dd hh.mm.ss`, as the name. Cancel drops the job. The receiver runs in the
  background, which Windows keeps from taking the focus; a helper thread joins the foreground
  window's input for a moment to make the dialog active, and keeps it on top while it is open.
  `--out-dir <folder>` saves each job there as `Printed yyyy-mm-dd hh.mm.ss.pdf` without asking
  (the end-to-end test uses it).
- **The installer** shows a checkbox ("Install the ASD PDF Printer", on by default).
  `msiexec /i … INSTALLPDFPRINTER=0` leaves it out; uninstall removes the queue and the port.

## Why the text layer is repaired

*Microsoft Print To PDF* draws glyph ids and gives `/ToUnicode` the text of the glyphs it kept.
For Thai it leaves out the glyphs the font's GSUB substitutes: a tone mark lowered or shifted left
over a tall letter or an upper vowel, a vowel over a letter that lost its tail. The page looks right
and the hidden text is `ทีตัง` for `ที่ตั้ง`, so search, copy, text-to-speech, and editing in any
viewer lose the tone marks. `asdreader_fonts::thai_repair` rewrites the `/ToUnicode` of such fonts
from the embedded font program itself (`glyph_text`: `cmap`, GSUB single, alternate and ligature
rules read backwards, and the Unicode-ordered runs of Thai glyphs a subset keeps), and the page text
reader joins a split SARA AM (`นํ้า`) into `น้ำ`.

The same repair is the engine edit `Edit::RepairThaiText` ("Repair Thai text"; `asdreader-cli edit
in.pdf --repair-thai --out out.pdf`), for PDFs printed some other way.

## Checking it

`packaging/windows/test-pdf-printer.ps1` runs end to end on a Windows machine (the release workflow
runs it on a runner): it adds the queue, starts the receiver, prints a page of Thai in Tahoma and
Leelawadee UI, and requires every printed line in the saved PDF, with no invented code points. The
MSI itself is checked by `test-msi.ps1`.

## Limits

- Windows 10 or later, one signed-in user at a time: the receiver of the first user to sign in has
  the port.
- Any program on the computer can send a PDF to the port; it brings up the receiving user's Save
  dialog, and a file saved is opened by ASDReader's hardened reader.
- One job at a time: a second job waits while the Save dialog of the first is open.
- The look of the printed page, fonts and print quality are the driver's; only the text layer is
  repaired.
