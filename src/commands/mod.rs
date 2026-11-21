//! Command dispatch.

mod build;
mod clean;
mod down;
mod plan;
mod recycle;
mod status;
mod tools;
mod up;
mod watch;

use crate::{
    cli::{Cli, Cmd},
    config,
    models::FleetConfig,
    state,
};
use anyhow::{Context, Result};
use std::{collections::HashSet, path::Path};

/// Execute `cli` and return the command exit code.
///
/// # Errors
/// Returns an error if configuration, locking, or command execution fails.
pub(crate) fn run(cli: Cli) -> Result<u8> {
    let _lock = if matches!(
        &cli.cmd,
        Cmd::Up { .. } | Cmd::Down { .. } | Cmd::Recycle { .. } | Cmd::Watch { .. }
    ) {
        Some(state::lock()?)
    } else {
        None
    };
    match cli.cmd {
        Cmd::PrepareTools {
            host,
            manifest,
            root,
        } => tools::prepare(&host, &manifest, &root).map(|_| 0),
        Cmd::InstallTools { manifest } => tools::install(&manifest).map(|_| 0),
        Cmd::Build { image, as_name } => {
            build::run(&cli.config, image.as_deref(), as_name.as_deref())
        }
        Cmd::Plan => load_cfg(&cli.config).and_then(|c| plan::run(&c)),
        Cmd::Up { names, force } => {
            load_cfg(&cli.config).and_then(|c| up::run(&c, names_filter(&names), force))
        }
        Cmd::Watch {
            names,
            interval,
            force,
        } => load_cfg(&cli.config)
            .and_then(|c| watch::run(&c, names_filter(&names), interval, force)),
        Cmd::Down { names, destroy } => {
            load_cfg(&cli.config).and_then(|c| down::run(&c, names_filter(&names), destroy))
        }
        Cmd::Recycle { names, force } => {
            load_cfg(&cli.config).and_then(|c| recycle::run(&c, names_filter(&names), force))
        }
        Cmd::Status => load_cfg(&cli.config).and_then(|_| status::run()),
        Cmd::Clean { goldens } => load_cfg(&cli.config).and_then(|c| clean::run(&c, goldens)),
    }
}

fn load_cfg(path: &Path) -> Result<FleetConfig> {
    config::load(path).with_context(|| format!("loading {}", path.display()))
}

fn names_filter(names: &[String]) -> Option<HashSet<String>> {
    (!names.is_empty()).then(|| names.iter().cloned().collect())
}
