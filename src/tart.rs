//! Thin wrappers around the `tart` CLI.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TartError {
    #[error("spawning `tart {subcmd}`")]
    Spawn {
        subcmd: String,
        #[source]
        source: io::Error,
    },
    #[error("`tart {subcmd}` failed: {stderr}")]
    Command { subcmd: String, stderr: String },
    #[error("parsing `tart list --format json`")]
    Json(#[from] serde_json::Error),
    #[error("opening log file {path}")]
    OpenLog {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("creating log dir {path}")]
    CreateLogDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, TartError>;

#[derive(Debug, Deserialize)]
struct VmEntry {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "State")]
    state: Option<String>,
    #[serde(rename = "Disk")]
    disk: Option<u64>,
}

fn tart_vms_dir() -> PathBuf {
    std::env::var_os("TART_HOME")
        .map(|v| PathBuf::from(v).join("vms"))
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
            PathBuf::from(home).join(".tart").join("vms")
        })
}

fn list() -> Result<Vec<VmEntry>> {
    let out = Command::new("tart")
        .args(["list", "--format", "json"])
        .output()
        .map_err(|source| TartError::Spawn {
            subcmd: "list".into(),
            source,
        })?;
    if !out.status.success() {
        return Err(TartError::Command {
            subcmd: "list".into(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn find<'a>(vms: &'a [VmEntry], name: &str) -> Option<&'a VmEntry> {
    vms.iter().find(|v| v.name == name && v.source == "local")
}

pub fn exists(name: &str) -> Result<bool> {
    Ok(find(&list()?, name).is_some())
}

pub fn is_running(name: &str) -> Result<bool> {
    Ok(find(&list()?, name)
        .and_then(|vm| vm.state.as_deref())
        .is_some_and(|s| s == "running"))
}

pub fn clone(source: &str, dest: &str) -> Result<()> {
    run("clone", ["clone", source, dest])
}

pub fn set_resources(name: &str, cpu: u32, memory_gb: u32, disk_gb: Option<u32>) -> Result<()> {
    let memory_mb = u64::from(memory_gb) * 1024;
    let mut args: Vec<String> = vec![
        "set".into(),
        name.into(),
        "--cpu".into(),
        cpu.to_string(),
        "--memory".into(),
        memory_mb.to_string(),
    ];
    if let Some(want) = disk_gb {
        // tart can only grow disks; treat the requested size as a floor.
        match disk_gb_of(name)? {
            Some(cur) if u64::from(want) <= cur => {
                println!("[{name}] disk already {cur}G (>= requested {want}G); keeping");
            }
            _ => args.extend(["--disk-size".to_string(), want.to_string()]),
        }
    }
    run("set", args.iter().map(String::as_str))
}

fn disk_gb_of(name: &str) -> Result<Option<u64>> {
    Ok(find(&list()?, name).and_then(|vm| vm.disk))
}

/// Cap on per-VM tart log size. Beyond this we rotate to `<log>.1` so a
/// long-running VM with chatty kernel/sysd output doesn't slowly fill the
/// host. One generation is enough — the prior boot's tail is preserved
/// while the current boot keeps appending fresh.
const MAX_LOG_BYTES: u64 = 50 * 1024 * 1024;

/// Launch a VM detached from this process. Returns the tart PID.
pub fn start(name: &str, log_path: &Path) -> Result<u32> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TartError::CreateLogDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    rotate_log_if_large(log_path);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|source| TartError::OpenLog {
            path: log_path.to_path_buf(),
            source,
        })?;
    let log_err = log.try_clone().map_err(|source| TartError::OpenLog {
        path: log_path.to_path_buf(),
        source,
    })?;

    let mut cmd = Command::new("tart");
    cmd.args(["run", "--no-graphics", name])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // SAFETY: setsid() is async-signal-safe and has no effect beyond the
    // child's process group. Ctrl-C on the CLI won't cascade to the VM.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|source| TartError::Spawn {
        subcmd: "run".into(),
        source,
    })?;
    Ok(child.id())
}

/// Best-effort log rotation: rename `<log>` -> `<log>.1` if it exceeds
/// `MAX_LOG_BYTES`. Any prior `.1` is overwritten — we keep one generation,
/// not a full archive. Failures are silent: the worst case is the next
/// `start` opens an oversized log, which is the existing behavior.
fn rotate_log_if_large(log_path: &Path) {
    let Ok(meta) = std::fs::metadata(log_path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let mut rotated = log_path.as_os_str().to_owned();
    rotated.push(".1");
    let _ = std::fs::rename(log_path, &rotated);
}

/// `tart prune --entries caches --older-than <days>`: evict OCI/IPSW cache
/// entries not accessed in the window. Local VMs are separate APFS clones and
/// are unaffected; the only cost is a re-pull on the next bake.
pub fn prune_caches(older_than_days: u32) -> Result<()> {
    let days = older_than_days.to_string();
    run(
        "prune",
        ["prune", "--entries", "caches", "--older-than", &days],
    )
}

/// Best-effort stop; the VM may already be stopped.
pub fn stop(name: &str) {
    let _ = Command::new("tart").args(["stop", name]).status();
}

/// Best-effort delete.
pub fn delete(name: &str) {
    let _ = Command::new("tart").args(["delete", name]).status();
}

/// Short fingerprint for a local VM based on its `config.json` mtime. Changes
/// whenever the VM is rebuilt, re-baked, or `tart set` runs — which is
/// exactly what we want for detecting golden drift on child clones. Returns
/// `None` if the VM doesn't exist locally.
pub fn fingerprint(name: &str) -> Option<String> {
    let config = tart_vms_dir().join(name).join("config.json");
    let mtime = std::fs::metadata(&config).ok()?.modified().ok()?;
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(secs.to_string())
}

pub fn ip(name: &str, timeout_s: u32) -> Result<String> {
    let out = Command::new("tart")
        .args(["ip", name, "--wait", &timeout_s.to_string()])
        .output()
        .map_err(|source| TartError::Spawn {
            subcmd: "ip".into(),
            source,
        })?;
    if !out.status.success() {
        return Err(TartError::Command {
            subcmd: format!("ip {name}"),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn run<'a, I>(subcmd: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let status = Command::new("tart")
        .args(args)
        .status()
        .map_err(|source| TartError::Spawn {
            subcmd: subcmd.into(),
            source,
        })?;
    if !status.success() {
        return Err(TartError::Command {
            subcmd: subcmd.into(),
            stderr: format!("exit {status}"),
        });
    }
    Ok(())
}
