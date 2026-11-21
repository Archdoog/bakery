//! Down command.

use crate::{
    github,
    models::FleetConfig,
    paths::state_dir,
    provision,
    runner::{GUEST_PASS, GUEST_USER},
    state::{state_file, tracked_state},
    tart,
};
use anyhow::Result;
use std::{collections::HashSet, fs};

/// Stop runners from `cfg` selected by `only`. Delete their VMs if `destroy` is true.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig, only: Option<HashSet<String>>, destroy: bool) -> Result<u8> {
    if !state_dir().exists() {
        println!("no runners tracked");
        return Ok(0);
    }

    // Host cleanup must work without GitHub access.
    let pat = cfg.pat().ok();

    for state in tracked_state()? {
        let name = &state.name;
        if let Some(f) = &only {
            if !f.contains(name) && !f.contains(&state.group) {
                continue;
            }
        }

        println!("[{name}] tearing down...");

        if let (false, Some(pat)) = (state.ip.is_empty(), pat.as_deref()) {
            match github::removal_token(&cfg.github.org, pat) {
                Ok(token) => {
                    if let Err(e) = provision::remove(&state.ip, GUEST_USER, GUEST_PASS, &token) {
                        println!("[{name}] removal warning: {e}");
                    }
                }
                Err(e) => println!("[{name}] removal warning: {e}"),
            }
        }

        // A failed guest removal can leave a GitHub registration.
        if let Some(pat) = pat.as_deref() {
            match github::find_runner(&cfg.github.org, pat, name) {
                Ok(Some(r)) => {
                    if let Err(e) = github::delete_runner(&cfg.github.org, pat, r.id) {
                        println!("[{name}] API prune warning: {e}");
                    }
                }
                Ok(None) => {}
                Err(e) => println!("[{name}] API prune warning: {e}"),
            }
        }

        tart::stop(name);
        if destroy {
            tart::delete(name);
            println!("[{name}] VM destroyed");
        }
        let _ = fs::remove_file(state_file(name));
        println!("[{name}] stopped");
    }
    Ok(0)
}
