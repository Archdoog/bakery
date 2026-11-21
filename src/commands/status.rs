//! Status command.

use crate::{state::tracked_state_if_any, tart};
use anyhow::Result;

/// Print the tracked runners and their VM status.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run() -> Result<u8> {
    let tracked = tracked_state_if_any()?;
    if tracked.is_empty() {
        println!("no runners tracked");
        return Ok(0);
    }
    for s in tracked {
        let running = tart::is_running(&s.name).unwrap_or(false);
        let ip = if s.ip.is_empty() { "?".into() } else { s.ip };
        println!(
            "{name:<22} group={group:<14} kind={kind:<5} ip={ip:<15} running={running}",
            name = s.name,
            group = s.group,
            kind = s.kind,
            running = if running { "yes" } else { "no" },
        );
    }
    Ok(0)
}
