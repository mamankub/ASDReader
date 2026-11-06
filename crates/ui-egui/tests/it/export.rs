//! Export a PDF ▸ Image, Text and AutoCAD DXF from the real shell (egui_kittest).

use asdreader_ui_egui::ASDReaderApp;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 100] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj << /Type /Page /Parent 2 0 R >> endobj
trailer << /Root 1 0 R >>
%%EOF";

#[test]
fn export_dialogs_write_images_and_text() {
    let dir = std::env::temp_dir().join(format!("asdreader-export-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = ASDReaderApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app.export_dir_override = Some(d.to_string_lossy().into_owned());
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("export.image"));
    h.run_steps(2);
    h.get_by_label("Export to Image");
    h.get_by_label("Export").click();
    h.run_steps(4);
    assert!(dir.join("doc_page_1.png").exists() && dir.join("doc_page_2.png").exists());
    h.get_by_label_contains("Exported 2 images");
    assert!(h.state_mut().execute("export.text"));
    h.run_steps(2);
    h.get_by_label("Export").click();
    h.run_steps(4);
    assert!(dir.join("doc.txt").exists());
    // Export all images: this document has none, and says so.
    assert!(h.state_mut().execute("export.all_images"));
    h.run_steps(2);
    h.get_by_label("Export All Images");
    h.get_by_label("Export").click();
    h.run_steps(4);
    h.get_by_label_contains("Exported 0 images");
    // Convert ▸ AutoCAD DXF: a drawing per page, at the scale the page states (none: 1:1).
    assert!(h.state_mut().execute("export.dxf"));
    h.run_steps(2);
    h.get_by_label("Export to AutoCAD DXF");
    h.get_by_label("Editable text");
    h.get_by_label("Export").click();
    h.run_steps(4);
    for page in [1, 2] {
        let dxf = std::fs::read_to_string(dir.join(format!("doc_page_{page}.dxf"))).unwrap();
        assert!(dxf.starts_with("  0\r\nSECTION\r\n  2\r\nHEADER\r\n") && dxf.ends_with("  0\r\nEOF\r\n"));
    }
    h.get_by_label_contains("Exported 2 drawings (scale 1:1)");
}

/// Convert opens on the Export a PDF tools, AutoCAD DXF among them.
#[test]
fn convert_lists_autocad_dxf() {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = ASDReaderApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(4);
    h.get_by_label("Convert").click();
    h.run_steps(4);
    h.get_by_label("AutoCAD drawing (.dxf)").click();
    h.run_steps(3);
    h.get_by_label("Export to AutoCAD DXF");
}
