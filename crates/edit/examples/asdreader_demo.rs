//! Synthetic ASDReader specimen, generated with the app's PDF writer and OFL fonts.
//! CRAFT_FONTS_DIR=../craft-fonts cargo run -p asdreader-edit --example asdreader_demo
use asdreader_cos::{Dict, Document, Object, SaveOptions};
use asdreader_edit::{AddedText, Content, add_content};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if asdreader_fonts::document_thai_font().is_none() {
        return Err("set CRAFT_FONTS_DIR to the pinned craft-fonts checkout (Noto Sans Thai is required)".into());
    }
    let mut doc = Document::new_empty();
    let pages = doc.add(Object::Dict(Dict::new()));
    let mut kids = Vec::new();
    for _ in 0..3 {
        let mut page = Dict::new();
        page.set(b"Type".to_vec(), Object::name("Page"));
        page.set(b"Parent".to_vec(), Object::Ref(pages));
        page.set(b"MediaBox".to_vec(), Object::Array([0, 0, 595, 842].map(Object::Int).to_vec()));
        kids.push(Object::Ref(doc.add(Object::Dict(page))));
    }
    doc.update_dict(pages, |d| {
        d.set(b"Type".to_vec(), Object::name("Pages"));
        d.set(b"Kids".to_vec(), Object::Array(kids));
        d.set(b"Count".to_vec(), Object::Int(3));
    })?;
    let mut catalog = Dict::new();
    catalog.set(b"Type".to_vec(), Object::name("Catalog"));
    catalog.set(b"Pages".to_vec(), Object::Ref(pages));
    let catalog = doc.add(Object::Dict(catalog));
    doc.trailer_mut().set(b"Root".to_vec(), Object::Ref(catalog));
    let sections: &[&[(&str, f64, f64)]] = &[
        &[
            ("ASDReader", 44.0, 68.0),
            ("อ่านชัด ทุกตัวอักษร", 30.0, 139.0),
            ("เอกสารตัวอย่างภาษาไทย", 18.0, 211.0),
            ("พยัญชนะ สระ และวรรณยุกต์", 20.0, 297.0),
            ("ก ข ฃ ค ฅ ฆ ง จ ฉ ช ซ ฌ ญ", 18.0, 340.0),
            ("ฎ ฏ ฐ ฑ ฒ ณ ด ต ถ ท ธ น บ ป ผ ฝ พ ฟ ภ ม", 18.0, 380.0),
            ("ย ร ล ว ศ ษ ส ห ฬ อ ฮ", 18.0, 420.0),
            ("น้ำ  ผู้ใช้  ปู่  ญู  กิ่  กี้  กึ้  กื๊  เก้า", 24.0, 493.0),
            ("ASDReader ภาษาไทย 2026", 18.0, 578.0),
            ("เลขไทย ๐๑๒๓๔๕๖๗๘๙ / English 0123456789", 18.0, 622.0),
            ("สร้างด้วย ASDReader และฟอนต์เปิด Noto Sans Thai", 12.0, 757.0),
        ],
        &[
            ("สระอยู่ครบ วรรณยุกต์อยู่ถูกที่", 25.0, 80.0),
            ("สระหน้า: เก้า แก้ โก้ ใส่ ไก่", 24.0, 169.0),
            ("สระบน: กิ กี กึ กื กิ่ กี้ กึ้ กื๊", 24.0, 225.0),
            ("สระล่าง: กุ กู ปุ ปู ปู่ ญุ ญู ฐุ ฐู", 24.0, 281.0),
            ("สระอำ: น้ำ ย้ำ ค้ำ ช้ำ ซ้ำ", 24.0, 337.0),
            ("สระหลัง: กะ กา กำ ฤๅ", 24.0, 393.0),
            ("ภาษาไทยสลับอังกฤษ ASDReader น้ำ 123", 20.0, 474.0),
            ("อ่าน พิมพ์ เพิ่มข้อความ บันทึก และเปิดกลับ", 20.0, 548.0),
            ("ตัวอย่างนี้ไม่มีข้อมูลส่วนบุคคลหรือเอกสารจากภายนอก", 13.0, 757.0),
        ],
        &[
            ("พื้นที่อ่านของคุณ", 32.0, 80.0),
            ("เอกสารทุกหน้าอยู่ในมือคุณ", 24.0, 160.0),
            ("จัดหน้า หมุนหน้า รวมไฟล์ และแยกเอกสาร", 20.0, 239.0),
            ("ค้นหาและคัดลอกข้อความในเอกสารที่มี text layer", 19.0, 307.0),
            ("โหมดสว่างและโหมดมืดสำหรับการอ่าน", 20.0, 375.0),
            ("สร้างจาก ASDReader ด้วย Rust และ egui", 18.0, 474.0),
            ("github.com/mamankub/ASDReader", 16.0, 757.0),
        ],
    ];
    for (page, lines) in sections.iter().enumerate() {
        for &(text, size, y) in *lines {
            let color = if y < 150.0 { [1.0, 0.09, 0.27] } else { [0.13, 0.11, 0.15] };
            // AddedText uses PDF coordinates: the top of the box is measured from the bottom.
            let top = 842.0 - y;
            let t = AddedText { text: text.into(), rect: [50.0, top - 60.0, 545.0, top], size, color, ..Default::default() };
            add_content(&mut doc, page, &Content::Text(t))?;
        }
    }
    let path = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "dist/demo/asdreader-thai.pdf".into()));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, asdreader_cos::write_full(&doc, &SaveOptions::default())?)?;
    println!("{}", path.display());
    Ok(())
}
