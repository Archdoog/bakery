//! Up command.

use crate::{
    host,
    models::{runner_names, FleetConfig},
    paths::{logs_dir, state_dir},
    runner::bring_up,
};
use anyhow::{Context, Result};
use std::{collections::HashSet, fs};

/// Start runners from `cfg` selected by `only`. Use `force` to skip the resource budget check.
///
/// # Errors
/// Returns an error if a required host, configuration, or service operation fails.
pub(super) fn run(cfg: &FleetConfig, only: Option<HashSet<String>>, force: bool) -> Result<u8> {
    fs::create_dir_all(state_dir()).context("creating state dir")?;
    fs::create_dir_all(logs_dir()).context("creating logs dir")?;

    let hr = host::detect()?;
    let problems = host::check_fit(hr, cfg.reserve(), cfg.groups(), host::DEFAULT_OVERCOMMIT);
    if !problems.is_empty() {
        if force {
            println!("Fleet does NOT fit (proceeding with --force):");
            for p in &problems {
                println!("  ! {p}");
            }
        } else {
            println!("Fleet does NOT fit; aborting. Run `plan` for details, or pass --force.");
            for p in &problems {
                println!("  ! {p}");
            }
            return Ok(1);
        }
    }

    let pat = cfg.pat()?;

    let targets = cfg.groups().iter().flat_map(|g| {
        runner_names(g)
            .map(move |n| (g, n))
            .filter(|(g, n)| match &only {
                Some(f) => f.contains(n) || f.contains(&g.name),
                None => true,
            })
    });

    for (g, name) in targets {
        if let Err(e) = bring_up(cfg, g, &name, &pat) {
            eprintln!("[{name}] FAILED: {e:#}");
            return Ok(2);
        }
    }
    Ok(0)
}
