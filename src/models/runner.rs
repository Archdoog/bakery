//! Per-runner-group schema: kind, resources, labels.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Macos,
    Linux,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Macos => "macos",
            Kind::Linux => "linux",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Resources {
    pub cpu: u32,
    pub memory_gb: u32,
    #[serde(default)]
    pub disk_gb: u32,
}

impl Resources {
    /// `disk_gb == 0` means "no floor — inherit whatever tart gives us".
    pub fn disk_floor(&self) -> Option<u32> {
        (self.disk_gb > 0).then_some(self.disk_gb)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerGroup {
    pub name: String,
    pub kind: Kind,
    pub image: String,
    pub count: u32,
    pub resources: Resources,
    pub labels: Vec<String>,
}

/// Expand a group into its per-runner names: `ubuntu` with count=2 → `ubuntu-1`, `ubuntu-2`.
pub fn runner_names(group: &RunnerGroup) -> impl Iterator<Item = String> + '_ {
    (1..=group.count).map(|i| format!("{}-{i}", group.name))
}
