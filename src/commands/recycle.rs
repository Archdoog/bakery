//! Recycle command.

use crate::models::FleetConfig;
use anyhow::Result;
use std::collections::HashSet;

/// Replace runners from `cfg` selected by `only`. Use `force` to skip the resource budget check.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig, only: Option<HashSet<String>>, force: bool) -> Result<u8> {
    let rc = super::down::run(cfg, only.clone(), true)?;
    if rc != 0 {
        return Ok(rc);
    }
    super::up::run(cfg, only, force)
}
