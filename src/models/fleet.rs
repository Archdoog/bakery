//! Top-level `runners.yaml` schema.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::runner::RunnerGroup;

pub const DEFAULT_RUNNER_VERSION: &str = "latest";

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
    pub root_images: BTreeMap<String, String>,
    #[serde(default)]
    pub goldens: BTreeMap<String, GoldenConfig>,
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
    /// Resolve the root for bake `image` and `destination`. A golden source overrides the shared root.
    /// Remote references without a tag use `latest`. Local names and pinned tags or digests remain unchanged.
    pub(crate) fn root_image(&self, image: &str, destination: &str) -> Option<String> {
        self.goldens
            .get(destination)
            .and_then(|golden| golden.source.as_deref())
            .or_else(|| self.root_images.get(image).map(String::as_str))
            .or(default_root(image))
            .map(|source| {
                if source.contains('/')
                    && !source.contains('@')
                    && !source.rsplit('/').next().unwrap_or(source).contains(':')
                {
                    format!("{source}:latest")
                } else {
                    source.to_owned()
                }
            })
    }

    pub fn reserve(&self) -> Reserve {
        self.host.reserve
    }

    pub fn groups(&self) -> &[RunnerGroup] {
        &self.runners
    }

    /// Read the GitHub PAT from the env var named by `github.token_env`.
    pub fn pat(&self) -> Result<String, PatMissing> {
        std::env::var(&self.github.token_env).map_err(|_| PatMissing(self.github.token_env.clone()))
    }
}

/// Return the latest published root for a built-in bake image.
pub(crate) fn default_root(image: &str) -> Option<&'static str> {
    match image {
        "macos" => Some("ghcr.io/cirruslabs/macos-golden-gate-xcode:latest"),
        "ubuntu" => Some("ghcr.io/cirruslabs/ubuntu:latest"),
        _ => None,
    }
}

/// Defines a golden image source and optional tool overrides.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenConfig {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub xcodes: Vec<String>,
    #[serde(default)]
    pub simulators: Vec<SimulatorConfig>,
    #[serde(default)]
    pub android_packages: Vec<String>,
}

/// Selects an iOS runtime and device names from the simulator catalog.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorConfig {
    pub version: String,
    #[serde(default)]
    pub devices: Vec<String>,
}
