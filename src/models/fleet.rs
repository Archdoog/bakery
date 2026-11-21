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

/// Defines the Apple development tools and simulator devices for a golden image.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenConfig {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub xcodes: Vec<XcodeConfig>,
    #[serde(default)]
    pub default_xcode: Option<String>,
    #[serde(default)]
    pub simulators: Vec<SimulatorConfig>,
}

/// Selects a platform runtime, its source image, and its simulator devices.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorConfig {
    #[serde(default)]
    pub xcode: Option<String>,
    pub platform: SimulatorPlatform,
    pub version: String,
    #[serde(default)]
    pub devices: Vec<SimulatorDevice>,
    #[serde(default)]
    pub runtime_image: Option<String>,
    #[serde(default)]
    pub donor_source: Option<String>,
}

/// Names a platform that Xcode can download for a simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum SimulatorPlatform {
    #[serde(rename = "iOS")]
    Ios,
    #[serde(rename = "watchOS")]
    Watchos,
    #[serde(rename = "tvOS")]
    Tvos,
    #[serde(rename = "visionOS")]
    Visionos,
}

/// Selects a simulator device name and a CoreSimulator device type identifier.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorDevice {
    pub name: String,
    pub device_type: String,
}

/// Selects an Xcode version and an optional Tart image that contains it.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct XcodeConfig {
    pub version: String,
    #[serde(default)]
    pub source: Option<String>,
}

impl XcodeConfig {
    /// Return the Tart source image for this Xcode version.
    pub fn source_image(&self) -> String {
        self.source
            .clone()
            .unwrap_or_else(|| format!("ghcr.io/cirruslabs/macos-tahoe-xcode:{}", self.version))
    }
}
