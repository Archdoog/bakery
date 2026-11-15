//! Build command.

use crate::paths::scripts_dir;
use anyhow::{Context, Result};
use std::{fs, process::Command};

/// Build `image` as `as_name`, or list available images if `image` is absent.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(image: Option<&str>, as_name: Option<&str>) -> Result<u8> {
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
    let status = Command::new(&script)
        .arg(dest)
        .status()
        .with_context(|| format!("spawning {}", script.display()))?;
    Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
}
