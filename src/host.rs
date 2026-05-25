//! Host resource detection and fleet feasibility checks.

use std::io;
use std::num::ParseIntError;
use std::process::Command;

use thiserror::Error;

use crate::models::{Kind, Reserve, RunnerGroup};

/// Apple's EULA caps concurrent macOS guests at 2 per host.
pub const MACOS_VM_CAP: u32 = 2;

/// Default soft over-commit allowance for CPU and RAM. Self-hosted runners
/// are usually idle and rarely all pinned simultaneously, so a strict
/// 1.0× host budget rejects fleets that work fine in practice. 1.5× is
/// the empirical headroom this host has been running at.
pub const DEFAULT_OVERCOMMIT: f64 = 1.5;

#[derive(Debug, Error)]
pub enum Error {
    #[error("spawning sysctl")]
    Spawn(#[from] io::Error),
    #[error("sysctl -n {key} failed: {stderr}")]
    Sysctl { key: String, stderr: String },
    #[error("parsing sysctl {key} output {value:?}")]
    Parse {
        key: String,
        value: String,
        #[source]
        source: ParseIntError,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct HostResources {
    pub cpu: u32,
    pub memory_gb: u32,
}

pub fn detect() -> Result<HostResources, Error> {
    let cpu: u32 = parse_sysctl("hw.ncpu")?;
    let mem_bytes: u64 = parse_sysctl("hw.memsize")?;
    Ok(HostResources {
        cpu,
        memory_gb: (mem_bytes / (1024 * 1024 * 1024)) as u32,
    })
}

/// Effective per-resource budget after reserve and over-commit allowance.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub cpu: u32,
    pub memory_gb: u32,
}

pub fn budget(host: HostResources, reserve: Reserve, overcommit: f64) -> Budget {
    let avail_cpu = host.cpu.saturating_sub(reserve.cpu);
    let avail_mem = host.memory_gb.saturating_sub(reserve.memory_gb);
    Budget {
        cpu: ((avail_cpu as f64) * overcommit).floor() as u32,
        memory_gb: ((avail_mem as f64) * overcommit).floor() as u32,
    }
}

pub fn check_fit(
    host: HostResources,
    reserve: Reserve,
    groups: &[RunnerGroup],
    overcommit: f64,
) -> Vec<String> {
    let mut problems = Vec::new();

    let total_cpu: u32 = groups.iter().map(|g| g.count * g.resources.cpu).sum();
    let total_mem: u32 = groups.iter().map(|g| g.count * g.resources.memory_gb).sum();
    let b = budget(host, reserve, overcommit);

    if total_cpu > b.cpu {
        problems.push(format!(
            "CPU over budget: fleet wants {total_cpu}, host allows {budget_cpu} \
             ({host_cpu} - {reserve_cpu} reserved, {overcommit:.2}x over-commit)",
            budget_cpu = b.cpu,
            host_cpu = host.cpu,
            reserve_cpu = reserve.cpu,
        ));
    }
    if total_mem > b.memory_gb {
        problems.push(format!(
            "RAM over budget: fleet wants {total_mem} GB, host allows {budget_mem} GB \
             ({host_mem} - {reserve_mem} reserved, {overcommit:.2}x over-commit)",
            budget_mem = b.memory_gb,
            host_mem = host.memory_gb,
            reserve_mem = reserve.memory_gb,
        ));
    }

    let macos_count: u32 = groups
        .iter()
        .filter(|g| g.kind == Kind::Macos)
        .map(|g| g.count)
        .sum();
    if macos_count > MACOS_VM_CAP {
        problems.push(format!(
            "macOS VM count {macos_count} exceeds Apple's {MACOS_VM_CAP}-VM per-host cap"
        ));
    }

    problems
}

fn parse_sysctl<T>(key: &str) -> Result<T, Error>
where
    T: std::str::FromStr<Err = ParseIntError>,
{
    let out = Command::new("sysctl").args(["-n", key]).output()?;
    if !out.status.success() {
        return Err(Error::Sysctl {
            key: key.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    value.parse().map_err(|source| Error::Parse {
        key: key.to_string(),
        value,
        source,
    })
}
