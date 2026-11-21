//! Build command.

use crate::paths::scripts_dir;
use anyhow::{Context, Result};
use std::{fs, path::Path, process::Command};

/// Build `image` as `as_name` with `config_path`, or list images if `image` is absent.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(config_path: &Path, image: Option<&str>, as_name: Option<&str>) -> Result<u8> {
    let dir = scripts_dir();
    let mut available: Vec<String> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("bake-")
                .and_then(|s| s.strip_suffix(".sh"))
                .map(str::to_string)
        })
        .collect();
    available.sort();

    let Some(image) = image else {
        if available.is_empty() {
            eprintln!("no bake scripts found in scripts/");
            return Ok(1);
        }
        println!("Available goldens:");
        for n in &available {
            println!("  - {n}");
        }
        println!("\nRun: bakery build <name>");
        return Ok(0);
    };

    let script = dir.join(format!("bake-{image}.sh"));
    if !script.exists() {
        eprintln!("no bake script for {image:?}");
        if !available.is_empty() {
            eprintln!("available: {}", available.join(", "));
        }
        return Ok(1);
    }

    let dest = as_name.unwrap_or(image);
    println!(
        ">>> building golden {dest:?} via {}",
        script.file_name().and_then(|s| s.to_str()).unwrap_or("")
    );
    let mut command = Command::new(&script);
    command.arg(dest);
    if image == "macos" {
        let cfg = super::load_cfg(config_path)?;
        let golden = cfg.goldens.get(dest).cloned().unwrap_or_default();
        if std::env::var_os("BAKE_SOURCE").is_none() {
            if let Some(source) = golden
                .source
                .clone()
                .or_else(|| golden.xcodes.first().map(|xcode| xcode.source_image()))
            {
                command.env("BAKE_SOURCE", source);
            }
        }
        command.env("BAKERY_APPLE_JSON", serde_json::to_string(&golden)?);
        command.env("BAKERY_BIN", std::env::current_exe()?);
        command.env(
            "BAKERY_SIMULATORS_JSON",
            serde_json::to_string(&golden.simulators)?,
        );
    }
    let status = command
        .status()
        .with_context(|| format!("spawning {}", script.display()))?;
    Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
}
