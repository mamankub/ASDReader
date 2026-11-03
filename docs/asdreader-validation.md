# ผลตรวจ ASDReader: แบรนด์และภาษาไทย

สถานะ: ผ่านการตรวจในเครื่อง; เตรียมเผยแพร่รุ่นแรก v0.13.1 ตามอนุมัติเจ้าของ — 2026-11-03

ฐานซอร์ส `a2ec27b4775f53c40cad3456936d9406611e3a8d` บน branch `feat/asdreader-brand-thai` ใช้ Rust 1.95.0 และ craft-fonts revision `3d9ab3a132ed0eff457e424226a435a901da30d3` ตรวจ SHA-256 ตรงกับ manifest ครบทั้ง 7 ไฟล์ฟอนต์ ไม่มีฟอนต์ใหม่ถูกคัดลอกเข้า repository นี้

## การเปลี่ยนแปลง

- logo/icon รูปเอกสารและตัว A สีแดงนีออน `#FF1744` กับขาว ใช้ใน UI, wordmark, favicon, PNG, ICO และ ICNS พร้อมที่มาและสิทธิ์ใน `ATTRIBUTION.toml`
- หน้าแรก About ชื่อหน้าต่าง และลิงก์ Help/updates ใช้ ASDReader โดยเก็บ package ID และ command ID เดิมเพื่อความเข้ากันได้
- README หลักเป็นภาษาไทย ภาพมาจาก UI จริงของ ASDReader และ PDF สังเคราะห์ที่สร้างด้วย `crates/edit/examples/asdreader_demo.rs`
- แยกช่วงอักษรไทยก่อนจัดรูปข้อความจากฟอนต์เอกสาร แก้สระอำและตำแหน่งเครื่องหมายในข้อความอังกฤษสลับไทย
- ขณะเพิ่มข้อความ เก็บสระหน้าและสระหลังไว้กับตัวฐานในการขึ้นบรรทัดใหม่ รวมถึงการเก็บเครื่องหมายซ้อนด้วย shaped clusters ที่มีอยู่เดิม
- `cargo xtask asdreader build|run|test` บังคับมีฟอนต์ไทยก่อน build; `CRAFT_FONTS_REQUIRED` ที่ตั้งไว้แต่ไม่มี `CRAFT_FONTS_DIR` จะหยุดด้วย error
- CI เดิมบังคับ `require-craft-fonts`, `ASDREADER_REQUIRE_THAI=1` และ `ASDREADER_SYSTEM_FONTS=0` ทั้ง Linux และ Windows โดยใช้ reusable workflow เดิม ไม่เพิ่ม actions หรือขั้นตอนติดตั้งแพ็กเกจใหม่
- ลบไฟล์ trademark ArtCraft เดิมครบ 8 ไฟล์ตามอนุมัติเจ้าของ และนำรายการออกจาก manifest โดยคง `LICENSE-brand.txt` และเครดิตต้นทางไว้
- จัดรูป guard ของ ClosePath ใน `crates/edit/src/images.rs` และย่อ boolean ใน `crates/ui-egui/src/compare_ui.rs` ให้ผ่าน lint โดยคงพฤติกรรมเดิม

## หลักฐานก่อนและหลังแก้

สอง regression ต่อไปนี้ถูกเรียกจริงก่อนแก้ และพบ failure:

1. `thai_wrapping_keeps_leading_and_spacing_vowels_with_their_base`: คำว่า `เก้า` แตกเป็น `เก้` และ `า` เมื่อกล่องแคบ หลังแก้เก็บข้อความครบและไม่ขึ้นบรรทัดด้วยสระหลังหรือจบบรรทัดด้วยสระหน้า
2. `thai_after_latin_keeps_its_contextual_glyphs_and_mark_offsets`: ส่วน `น้ำ` ใน `ASD น้ำ` ให้ 3 glyphs ขณะที่การจัดรูปไทยเดี่ยวให้ 4 glyphs หลังแก้ glyph ID และตำแหน่งเครื่องหมายตรงกับผลภาษาไทยเดี่ยว โดย Unicode ไม่ซ้ำหรือหาย

ผลที่ตรวจแล้ว:

| การตรวจ | ผล |
|---|---|
| `cargo test -p asdreader-fonts -p asdreader-edit --lib -j 2 --locked` | ผ่าน: fonts 89 และ edit 250 tests |
| `cargo xtask asdreader test --lib` หลังปรับ lint | ผ่านครบ 339 tests รวม regression ของ path ว่าง; ยืนยันว่า helper เลือกฟอนต์และส่ง flags ได้จริง |
| `cargo test -p asdreader-engine --test it thai -j 2 --locked` | ผ่าน 1 test: ภาพหลัง save/reopen ตรงทุกพิกเซล, ข้อความคัดลอกได้ครบ, undo/redo คืนภาพถูกต้อง |
| `cargo test -p asdreader-ui-egui --features eframe/x11 --test it thai -j 2 --locked` | ผ่าน 2 tests: glyph ไทยครบในทุก UI family ที่ใช้ และพิมพ์ไทยลงหน้า PDF ได้ |
| UI link/update/viewer tests | ผ่าน 38 / 4 / 62 tests ตามลำดับ; update ใช้ข้อมูลจำลอง ไม่มีการตรวจ release จริง |
| UI shell/picker tests | ผ่าน 61 / 38 tests ตามลำดับ; ชุดที่ render ใช้ software Vulkan |
| UI compare tests หลังย่อ boolean | ผ่าน 7 tests รวมการกรองพื้นที่เปรียบเทียบและภาพหน้าจอ |
| `cargo fmt --all --check` | ผ่าน |
| Clippy: fonts, edit, engine, ui-egui, asdreader, xtask | ผ่าน `--all-targets --features eframe/x11 -j 2 --locked -- -D warnings` |
| `cargo xtask wasm` | ผ่าน 31 crates รวม UI; มี warnings ของโค้ด native ที่ไม่ถูกใช้บน wasm (render 4, ui-egui 7) |
| `cargo xtask layers` | ผ่าน 37 crates ไม่มีการข้ามชั้น dependency |
| `cargo xtask assets --write` และ `cargo xtask assets` | ผ่าน: 285 repository assets, 5 dependency assets, 22 fetched assets; สร้าง `ATTRIBUTION.md` แล้ว |
| `cargo test -p xtask assets::tests --locked -j 2` หลังลบโลโก้เก่า | ผ่าน 15 tests รวม manifest จริง; UI link/About tests ผ่านซ้ำอีก 38 tests |
| `actionlint 1.7.12 .github/workflows/ci.yml` | ผ่าน; ตรวจ inputs กับซอร์ส reusable workflow จริง และยังไม่ได้เรียก GitHub Actions |
| ฟอร์แมตไอคอน | ICO ทั้งสองไฟล์มี 9 ขนาด; ICNS มี PNG entries 7 ขนาด |
| ฟอนต์ขาดหาย | เรียก build script ด้วย required flag: ไม่มี directory และมี manifest ที่ไม่มี Thai ต่างให้ `cargo::error` |

ใช้ `CRAFT_FONTS_REQUIRED=1 ASDREADER_REQUIRE_THAI=1 ASDREADER_SYSTEM_FONTS=0` ในการตรวจชุดไทย จึงไม่อาศัยฟอนต์ระบบช่วยให้ผ่าน โดย UI coverage ครอบคลุม codepoint ไทยที่กำหนดใช้ `U+0E01..U+0E3A` และ `U+0E3F..U+0E5B` ที่ scale 1, 1.5 และ 2

## ตรวจซ้ำเพื่อเตรียม v0.13.1

หลังปรับเวอร์ชัน รัน fmt, scoped clippy, fonts/edit 339 tests, engine Thai 1 test, layers, assets และ wasm ผ่านอีกครั้ง ตรวจ Cargo.lock ว่าเปลี่ยนเฉพาะ 36 workspace versions และตรวจ actionlint สำหรับ CI/release, Bash syntax, PowerShell 7.6.9 parser และ font hashes ครบ 7 ไฟล์ผ่าน

## อุปสรรค release ที่พบจริง

Release candidate แรกคอมไพล์ Linux สำเร็จ แต่หยุดเมื่อดาวน์โหลด OCR `.rten` ได้ HTTP 403 ผู้พัฒนาประกาศเลิก URL เดิมใน [ocrs #282](https://github.com/robertknight/ocrs/issues/282) จึงใช้ ONNX จากสคริปต์ทางการที่ตรึง commit พร้อม hash และ license เดิม ตัวโหลดรองรับทั้งคู่ ONNX และคู่ RTEN โดยไม่ผสมรูปแบบ Regression `prefers_complete_onnx_pairs_and_keeps_legacy_models` ถูกเรียกและล้มก่อนแก้

ผลหลังแก้: ดาวน์โหลด ONNX และ license ผ่าน hash ทั้งหมด, OCR 11 tests ผ่านรวม `reads_rendered_text` โดยตั้ง `ASDREADER_MODELS` ไปที่โมเดลจริง, engine OCR 6 tests และ engine Thai 1 test ผ่าน; fmt, clippy ของ OCR/engine/xtask, assets, layers และ wasm 31 crates ผ่าน มี dependency เพิ่มเฉพาะ `rten-onnx 0.26.0` ตาม feature ของ RTen เดิม ไม่อัปเกรดแพ็กเกจอื่น

CI FreeBSD เดิมเริ่ม VM ไม่ได้ (`Cannot allocate memory`) ก่อนถึง build/test ของโค้ด จึงยังไม่มีผลรับรอง FreeBSD

## Dependency audit ก่อนเผยแพร่

[CI บน commit 9c03919](https://github.com/mamankub/ASDReader/actions/runs/41594502355) ผ่าน 4,616 tests (ข้าม 16 tests), doctests, fmt, workspace clippy, CLI without default features, layers, wasm และ assets แต่หยุดที่ `cargo deny` เนื่องจาก `crypto-bigint 0.7.5` มี [RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html)

ตรวจแพตช์ [upstream #4366](https://github.com/mamankub/ASDReader/pull/4366) และ changelog จากผู้พัฒนาแล้ว: รุ่น 0.7.6 ถูกถอนภายหลัง จึงใช้รุ่นที่ยังเผยแพร่ `crypto-bigint 0.7.10` พร้อม `ctutils 0.4.8` โดยเปลี่ยนเฉพาะสองแพ็กเกจนี้ใน Cargo.lock ผล `cargo-deny 0.20.2 --log-level error check` ผ่าน advisories, bans, licenses และ sources โดยไม่เพิ่มข้อยกเว้น หลังอัปเดต ทดสอบลายเซ็น 139 tests ผ่าน (ข้าม 1 test), clippy ของ sign/engine ทุก target แบบ `-D warnings`, fmt, layers 37 crates, assets, wasm 31 crates และ parity ผ่าน รุ่นเผยแพร่ต้องสร้างใหม่จาก commit ที่แก้แล้ว

## เอกสารสิทธิ์ในแพ็กเกจ

แพ็กเกจ archive แนบ ATTRIBUTION.md และไฟล์ใบอนุญาตที่ NOTICE อ้างถึงสำหรับ Inter, JetBrains Mono, Dancing Script, ฟอนต์เริ่มต้นของ egui, Lucide และทรัพยากร PDF ที่ฝังมากับแอป เพิ่มจากใบอนุญาต craft-fonts/โมเดล OCR ที่คัดลอกอยู่แล้ว สคริปต์ Linux และ Windows ใช้รายการเดียวกันใน `packaging/asset-licences.txt` และ NOTICE ระบุการเปลี่ยนแบรนด์พร้อมเก็บเครดิตต้นทาง ตรวจ staging helpers จริงทั้ง Bash และ PowerShell แล้ว: คัดลอกครบ 11 ไฟล์และ byte ตรงต้นฉบับ; ฝั่ง Linux คงใบอนุญาตฟอนต์ไทยและ ATTRIBUTION.md ครบ

## ภาพที่ตรวจด้วยตา

ใช้ `egui_kittest` กับ Mesa lavapipe เพื่อ render UI ของแอปจริง ดู [โหมดสว่าง](images/asdreader-thai.png), [โหมดมืด](images/asdreader-dark.png) และ [หน้าแรก](images/asdreader-home.png) วิธีสร้างซ้ำอยู่ใน README ไม่ใช่ภาพ mockup และไม่มีเอกสารหรือข้อมูลส่วนตัวของผู้ใช้

## ขอบเขตที่ยังไม่ยืนยัน

- ไม่ได้อ้างว่าทุก PDF หรือทุกฟอนต์ภายนอกถูกต้อง: PDF ที่ฝังฟอนต์ไม่ครบหรือไม่มี Unicode mapping อาจยังมีข้อจำกัด การกรอกฟอร์มและ annotation ใช้เส้นทางฟอนต์แยกจาก AddText
- ยังไม่มีการตัดคำไทยด้วยพจนานุกรม และไม่ได้เพิ่ม OCR ภาษาไทย
- ตรวจบน Linux; ไม่ได้เปิดแอปบน Windows/macOS จริงหรือทดสอบตัวติดตั้งและการเซ็นแพ็กเกจ
- Release workflow ปรับเป็น candidate builds สำหรับ Linux x86_64 และ Windows x64 portable โดยบังคับฟอนต์ไทยและตรวจ hash; ผล build และไฟล์ที่เผยแพร่ให้ตรวจจากหน้า release ของ v0.13.1
- ผลในตารางเป็นการตรวจในเครื่อง; ผล GitHub CI แยกตาม commit ในหัวข้อ Dependency audit และลิงก์ workflow ของ release ไม่ถือว่า CI รอบก่อนเป็นผลของ commit ใหม่

## การนำแบรนด์เก่าออก

ไฟล์ที่นำออกแล้ว: `artcraft-logo.svg`, `artcraft-logo.png`, `artcraft-logo-white.svg`, `artcraft-logo-white.png`, `artcraft-mark.svg`, `artcraft-mark.png`, `artcraft-mark-black.svg`, `artcraft-mark-black.png` ภายใน `docs/brand/` โดยคง `LICENSE-brand.txt` และเครดิตต้นทางไว้

## ประวัติ

| วันที่ | การเปลี่ยนแปลง |
|---|---|
| 2026-11-03 | บันทึกหลักฐาน regression ภาษาไทยและภาพ UI ของ ASDReader; ลบ trademark ตามอนุมัติเจ้าของ และตรวจ CI ที่บังคับฟอนต์ไทย |
