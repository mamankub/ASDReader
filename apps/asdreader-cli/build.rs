//! Windows only: embed version info (VERSIONINFO) into `asdreader-cli.exe`, so Explorer's
//! Details tab, `(Get-Item asdreader-cli.exe).VersionInfo` and inventory tools can read the version
//! and publisher, as they can for `ASDReader.exe` (`apps/asdreader/build.rs`).
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `ASDREADER_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ASDREADER_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "ASDReader")
        .set("FileDescription", "ASDReader command-line tool")
        .set("LegalCopyright", "Copyright (c) the ASDReader contributors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "asdreader-cli.exe")
        .set("InternalName", "asdreader-cli");
    if let Err(e) = res.compile() {
        if std::env::var_os("ASDREADER_REQUIRE_WINRES").is_some() {
            println!("cargo::error=embedding Windows resources failed: {e}");
            return;
        }
        println!("cargo:warning=asdreader-cli.exe built without version resources: {e}");
    }
}
