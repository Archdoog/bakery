//! Top-level `runners.yaml` schema.

use serde::Deserialize;
use thiserror::Error;

use super::runner::RunnerGroup;

pub const DEFAULT_RUNNER_VERSION: &str = "2.329.0";

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Reserve {
    #[serde(default = "default_reserve_cpu")]
    pub cpu: u32,
    #[serde(default = "default_reserve_mem")]
    pub memory_gb: u32,
}

fn default_reserve_cpu() -> u32 {
    4
}
fn default_reserve_mem() -> u32 {
    8
}

impl Default for Reserve {
    fn default() -> Self {
        Self {
            cpu: default_reserve_cpu(),
            memory_gb: default_reserve_mem(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct HostSection {
    #[serde(default)]
    pub reserve: Reserve,
    /// When set, `recycle` runs `tart prune --entries caches --older-than N`
    /// to evict OCI/IPSW cache entries not accessed in N days. The cache only
    /// exists to speed up re-pulls at bake time; goldens and runner VMs are
    /// independent local clones and are never touched by the prune.
    #[serde(default)]
    pub cache_retention_days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubConfig {
    pub org: String,
    #[serde(default = "default_token_env")]
    pub token_env: String,
}

fn default_token_env() -> String {
    "GH_PAT".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct FleetConfig {
    pub github: GitHubConfig,
    #[serde(default)]
    pub host: HostSection,
    pub runners: Vec<RunnerGroup>,
    #[serde(default = "default_runner_version")]
    pub runner_version: String,
}

fn default_runner_version() -> String {
    DEFAULT_RUNNER_VERSION.to_string()
}

#[derive(Debug, Error)]
#[error("GitHub PAT missing: export {0} (needs admin:org scope)")]
pub struct PatMissing(pub String);

impl FleetConfig {
    pub fn reserve(&self) -> Reserve {
        self.host.reserve
    }

    pub fn groups(&self) -> &[RunnerGroup] {
        &self.runners
    }

    /// Read the GitHub PAT from the env var named by `github.token_env`.
    pub fn pat(&self) -> Result<String, PatMissing> {
        std::env::var(&self.github.token_env)
            .map_err(|_| PatMissing(self.github.token_env.clone()))
    }
}
