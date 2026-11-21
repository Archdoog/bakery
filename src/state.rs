//! Runner state files and the fleet lock.

use crate::paths::state_dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Saved identity and VM details for one runner.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StateFile {
    pub(crate) name: String,
    pub(crate) vm: String,
    pub(crate) pid: u32,
    pub(crate) ip: String,
    pub(crate) kind: String,
    pub(crate) image: String,
    pub(crate) group: String,
    pub(crate) labels: Vec<String>,
    #[serde(default)]
    pub(crate) ephemeral: bool,
    #[serde(default)]
    pub(crate) golden_fingerprint: String,
    pub(crate) started_at: String,
}

/// Return the state file path for runner `name`.
pub(crate) fn state_file(name: &str) -> PathBuf {
    state_dir().join(format!("{name}.json"))
}

/// Read the saved state for runner `name`, or return `None` if absent.
///
/// # Errors
/// Returns an error if the file cannot be read or parsed.
pub(crate) fn state_read(name: &str) -> Result<Option<StateFile>> {
    let path = state_file(name);
    if !path.exists() {
        return Ok(None);
    }
    let txt = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let state: StateFile =
        serde_json::from_str(&txt).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(state))
}

/// Save `s` as the state for runner `name`.
///
/// # Errors
/// Returns an error if serialization or a file operation fails.
pub(crate) fn state_write(name: &str, s: &StateFile) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    let path = state_file(name);
    let txt = serde_json::to_string_pretty(s)?;
    fs::write(&path, txt).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read tracked runner states and report unreadable files.
///
/// # Errors
/// Returns an error if the state directory cannot be read.
pub(crate) fn tracked_state() -> Result<Vec<StateFile>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(state_dir())
        .context("reading state dir")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        match fs::read_to_string(&p)
            .map_err(anyhow::Error::from)
            .and_then(|s| -> Result<StateFile> { serde_json::from_str(&s).map_err(Into::into) })
        {
            Ok(s) => out.push(s),
            Err(e) => eprintln!("warn: skipping {}: {e:#}", p.display()),
        }
    }
    Ok(out)
}

/// Read tracked runner states, or return an empty list if the directory is absent.
///
/// # Errors
/// Returns an error if the state directory cannot be read.
pub(crate) fn tracked_state_if_any() -> Result<Vec<StateFile>> {
    if !state_dir().exists() {
        return Ok(vec![]);
    }
    tracked_state()
}

/// Lock fleet changes until the returned file closes.
///
/// # Errors
/// Returns an error if the lock file cannot be opened or another command holds the lock.
pub(crate) fn lock() -> Result<fs::File> {
    fs::create_dir_all(state_dir())?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir().join("fleet.lock"))?;
    file.try_lock().context(
        "Another fleet command holds the lock. Stop the watcher before you change the fleet.",
    )?;
    Ok(file)
}
