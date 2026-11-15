//! Watch command.

use crate::{
    host,
    models::{runner_names, FleetConfig},
    paths::logs_dir,
    runner::bring_up,
};
use anyhow::{bail, Result};
use std::{collections::HashSet, fs, thread::sleep, time::Duration};

/// Maintain ephemeral runners from `cfg` selected by `only`, with `interval` seconds between checks.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig, only: Option<HashSet<String>>, interval: u64) -> Result<u8> {
    let targets: Vec<_> = cfg
        .groups()
        .iter()
        .filter(|g| g.ephemeral)
        .flat_map(|g| runner_names(g).map(move |name| (g, name)))
        .filter(|(g, name)| {
            only.as_ref()
                .is_none_or(|f| f.contains(name) || f.contains(&g.name))
        })
        .collect();
    if targets.is_empty() {
        bail!("No ephemeral runners match the selection. Set ephemeral: true in runners.yaml.");
    }
    let hr = host::detect()?;
    let problems = host::check_fit(hr, cfg.reserve(), cfg.groups(), host::DEFAULT_OVERCOMMIT);
    if !problems.is_empty() {
        bail!("The fleet exceeds the host budget: {problems:?}");
    }
    let pat = cfg.pat()?;
    fs::create_dir_all(logs_dir())?;
    println!("Watching ephemeral runners. Stop this command before you run down or recycle.");
    loop {
        for (group, name) in &targets {
            if let Err(error) = bring_up(cfg, group, name, &pat) {
                eprintln!("[{name}] {error:#}. The watcher will retry.");
            }
        }
        sleep(Duration::from_secs(interval));
    }
}
