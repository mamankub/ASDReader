//! Windows only: embed the app icon and version info (VERSIONINFO) into `ASDReader.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab. A separate PDF document icon
//! uses resource ID 2; the installer references it as `ASDReader.exe,-2`.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `ASDREADER_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

#[path = "src/windows_manifest.rs"]
mod windows_manifest;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/asdreader.ico");
    println!("cargo:rerun-if-changed=../../assets/app-icon/asdreader-document.ico");
    println!("cargo:rerun-if-changed=src/windows_manifest.rs");
    println!("cargo:rerun-if-env-changed=ASDREADER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/asdreader.ico")
        .set_icon_with_id("../../assets/app-icon/asdreader-document.ico", "2")
        .set_manifest(windows_manifest::WINDOWS_MANIFEST)
        .set("ProductName", "ASDReader")
        .set("FileDescription", "ASDReader PDF workbench")
        .set("LegalCopyright", "Copyright (c) the ASDReader contributors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "ASDReader.exe")
        .set("InternalName", "asdreader");
    if let Err(e) = res.compile() {
        if std::env::var_os("ASDREADER_REQUIRE_WINRES").is_some() {
            println!("cargo::error=embedding Windows resources failed: {e}");
            return;
        }
        println!("cargo:warning=ASDReader.exe built without icon/version resources: {e}");
    }
}
