//! The supported ASDReader build entry point. Never silently ship without Thai fonts.
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub fn run(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("run");
    if !matches!(action, "build" | "run" | "test") {
        bail!("usage: cargo xtask asdreader build|run|test [cargo options] [-- app options]");
    }
    let root = crate::gates::root();
    let dir = std::env::var_os("ASDESIGN_FONTS_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("../asdesign-fonts"));
    let dir = dir.canonicalize().context("prepare the pinned asdesign-fonts checkout beside ASDReader, or set ASDESIGN_FONTS_DIR; see README.md")?;
    let mut command = Command::new("cargo");
    command.current_dir(root).env("ASDESIGN_FONTS_DIR", dir).env("ASDESIGN_FONTS_REQUIRED", "1").env("ASDREADER_REQUIRE_THAI", "1");
    command.args([action, "--locked", "-j", "2"]);
    if action == "test" {
        command.args(["-p", "asdreader-fonts", "-p", "asdreader-edit"]);
    } else {
        command.args(["-p", "asdreader"]);
    }
    command.args(args.get(1..).unwrap_or_default());
    let status = command.status().context("starting Cargo")?;
    if !status.success() {
        bail!("ASDReader {action} failed ({status})");
    }
    Ok(())
}
