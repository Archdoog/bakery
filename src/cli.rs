//! Command-line arguments.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Parsed command and fleet configuration path.
#[derive(Parser, Debug)]
#[command(
    name = "bakery",
    about = "Tart-backed GitHub Actions runner fleet for a single host."
)]
pub(crate) struct Cli {
    /// Path to the fleet YAML.
    #[arg(short = 'c', long = "config", default_value = "runners.yaml")]
    pub(crate) config: PathBuf,

    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

/// Available fleet commands.
#[derive(Subcommand, Debug)]
pub(crate) enum Cmd {
    #[command(hide = true)]
    PrepareTools {
        host: String,
        manifest: PathBuf,
        root: String,
    },
    #[command(hide = true)]
    InstallTools { manifest: PathBuf },
    /// Validate config and check host resource fit.
    Plan,
    /// Clone, start, and register runners.
    Up {
        /// Runner or group names (default: all).
        names: Vec<String>,
        /// Skip the resource budget check. The macOS guest limit still applies.
        #[arg(long)]
        force: bool,
    },
    /// Replace completed ephemeral runners. Keep this command active on the host.
    Watch {
        /// Runner or group names (default: all ephemeral groups).
        names: Vec<String>,
        /// Seconds between fleet checks.
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
        interval: u64,
        /// Proceed even if the fleet exceeds the host budget.
        #[arg(long)]
        force: bool,
    },
    /// Stop runners and remove them from GitHub.
    Down {
        /// Runner or group names (default: all).
        names: Vec<String>,
        /// Delete each VM after runner removal.
        #[arg(long)]
        destroy: bool,
    },
    /// Replace runners with fresh clones of their golden images.
    Recycle {
        /// Runner or group names (default: all).
        names: Vec<String>,
        /// Skip the resource budget check. The macOS guest limit still applies.
        #[arg(long)]
        force: bool,
    },
    /// List tracked runners.
    Status,
    /// Build a golden image, or list available images.
    Build {
        /// Golden name from scripts/bake-<name>.sh. Omit to list.
        image: Option<String>,
        /// Destination VM name (default: same as image)
        #[arg(long = "as")]
        as_name: Option<String>,
    },
    /// Delete Tart caches and keep golden images.
    Clean {
        /// Include golden images in delete.
        #[arg(long)]
        goldens: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_rejects_zero_interval() {
        assert!(Cli::try_parse_from(["bakery", "watch", "--interval", "0"]).is_err());
        assert!(Cli::try_parse_from(["bakery", "watch", "ubuntu", "--interval", "1"]).is_ok());
    }
}
