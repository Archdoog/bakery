//! Paths relative to the working directory.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // All path-relative lookups (scripts/, state/, logs/) resolve from the
    // invoking CWD, so the binary must be run from the repo root.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Return the scripts directory under the working directory.
pub(crate) fn scripts_dir() -> PathBuf {
    repo_root().join("scripts")
}

/// Return the state directory under the working directory.
pub(crate) fn state_dir() -> PathBuf {
    repo_root().join("state")
}

/// Return the logs directory under the working directory.
pub(crate) fn logs_dir() -> PathBuf {
    repo_root().join("logs")
}
