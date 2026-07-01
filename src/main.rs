//! Bakery CLI: bakes Tart goldens and stamps GitHub Actions runner VMs from them.
//!
//! Usage:
//!   bakery plan
//!   bakery up [NAME...] [--force]
//!   bakery down [NAME...] [--destroy]
//!   bakery recycle [NAME...] [--force]
//!   bakery status
//!   bakery build [IMAGE] [--as NAME]

mod config;
mod github;
mod host;
mod models;
mod provision;
mod tart;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use crate::models::{runner_names, FleetConfig, RunnerGroup};

/// Cirrus Labs image credentials; override per-VM if you've customized goldens.
const GUEST_USER: &str = "admin";
const GUEST_PASS: &str = "admin";

/// Seconds to wait for the runner agent to register online in GitHub after
/// provision finishes. The svc.sh-started agent typically registers within ~15s.
const HEALTHCHECK_TIMEOUT_S: u64 = 90;

fn repo_root() -> PathBuf {
    // All path-relative lookups (scripts/, state/, logs/) resolve from the
    // invoking CWD, so the binary must be run from the repo root.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn scripts_dir() -> PathBuf {
    repo_root().join("scripts")
}
fn state_dir() -> PathBuf {
    repo_root().join("state")
}
fn logs_dir() -> PathBuf {
    repo_root().join("logs")
}

#[derive(Parser, Debug)]
#[command(
    name = "bakery",
    about = "Tart-backed GitHub Actions runner fleet for a single host."
)]
struct Cli {
    /// Path to runners.yaml
    #[arg(short = 'c', long = "config", default_value = "runners.yaml")]
    config: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Validate config and check host resource fit.
    Plan,
    /// Clone, start, and register runners.
    Up {
        /// Runner or group names (default: all)
        names: Vec<String>,
        /// Skip the host fit check (Apple's macOS VM cap is still enforced)
        #[arg(long)]
        force: bool,
    },
    /// Stop runners and remove them from GitHub.
    Down {
        /// Runner or group names (default: all)
        names: Vec<String>,
        /// Also `tart delete` each VM so the next `up` re-clones fresh from the golden
        #[arg(long)]
        destroy: bool,
    },
    /// Destroy and re-create runners from their golden — wipes accumulated
    /// _work, caches, and DerivedData. Equivalent to `down --destroy` + `up`.
    /// Prunes the tart OCI cache when `host.cache_retention_days` is set.
    Recycle {
        /// Runner or group names (default: all)
        names: Vec<String>,
        /// Skip the host fit check on the `up` half (Apple's macOS VM cap is still enforced)
        #[arg(long)]
        force: bool,
    },
    /// List tracked runners.
    Status,
    /// Build a golden VM via scripts/bake-<image>.sh (no args: list available).
    Build {
        /// Golden name; must match scripts/bake-<name>.sh. Omit to list.
        image: Option<String>,
        /// Destination VM name (default: same as image)
        #[arg(long = "as")]
        as_name: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rc = match cli.cmd {
        // `build` is config-independent; load lazily for commands that need it.
        Cmd::Build { image, as_name } => cmd_build(image.as_deref(), as_name.as_deref()),
        Cmd::Plan => load_cfg(&cli.config).and_then(|c| cmd_plan(&c)),
        Cmd::Up { names, force } => {
            load_cfg(&cli.config).and_then(|c| cmd_up(&c, names_filter(&names), force))
        }
        Cmd::Down { names, destroy } => {
            load_cfg(&cli.config).and_then(|c| cmd_down(&c, names_filter(&names), destroy))
        }
        Cmd::Recycle { names, force } => {
            load_cfg(&cli.config).and_then(|c| cmd_recycle(&c, names_filter(&names), force))
        }
        Cmd::Status => load_cfg(&cli.config).and_then(|_| cmd_status()),
    };
    match rc {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::from(1)
        }
    }
}

fn load_cfg(path: &Path) -> Result<FleetConfig> {
    config::load(path).with_context(|| format!("loading {}", path.display()))
}

fn names_filter(names: &[String]) -> Option<HashSet<String>> {
    (!names.is_empty()).then(|| names.iter().cloned().collect())
}

fn cmd_plan(cfg: &FleetConfig) -> Result<u8> {
    let hr = host::detect()?;
    let reserve = cfg.reserve();
    let overcommit = host::DEFAULT_OVERCOMMIT;
    let b = host::budget(hr, reserve, overcommit);
    println!("Host:    {} cores, {} GB RAM", hr.cpu, hr.memory_gb);
    println!("Reserve: {} cores, {} GB RAM", reserve.cpu, reserve.memory_gb);
    println!(
        "Budget:  {} cores, {} GB RAM ({:.2}x over-commit)",
        b.cpu, b.memory_gb, overcommit,
    );
    println!(
        "Org:     {} (token from ${})\n",
        cfg.github.org, cfg.github.token_env
    );

    println!("Planned runners:");
    let (mut total_cpu, mut total_mem) = (0u32, 0u32);
    for g in cfg.groups() {
        for name in runner_names(g) {
            total_cpu += g.resources.cpu;
            total_mem += g.resources.memory_gb;
            let disk = match g.resources.disk_floor() {
                Some(d) => d.to_string(),
                None => "-".into(),
            };
            println!(
                "  - {name:<22} kind={kind:<5} image={image:<16} cpu={cpu} ram={ram}G disk={disk}G labels={labels}",
                kind = g.kind.as_str(),
                image = g.image,
                cpu = g.resources.cpu,
                ram = g.resources.memory_gb,
                labels = g.labels.join(","),
            );
        }
    }
    println!("\nTotal:   cpu={total_cpu} ram={total_mem}G");

    let problems = host::check_fit(hr, reserve, cfg.groups(), overcommit);
    if problems.is_empty() {
        println!("\nFleet fits within host budget.");
        return Ok(0);
    }
    println!("\nFleet does NOT fit:");
    for p in &problems {
        println!("  ! {p}");
    }
    Ok(1)
}

fn cmd_up(cfg: &FleetConfig, only: Option<HashSet<String>>, force: bool) -> Result<u8> {
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

    provision::ensure_sshpass()?;
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

fn bring_up(cfg: &FleetConfig, g: &RunnerGroup, name: &str, pat: &str) -> Result<()> {
    println!("[{name}] bringing up...");

    let golden_fp = tart::fingerprint(&g.image);
    reclone_if_drifted(name, &g.image, golden_fp.as_deref())?;

    if !tart::exists(name)? {
        println!("[{name}] cloning from {}", g.image);
        tart::clone(&g.image, name)?;
        tart::set_resources(
            name,
            g.resources.cpu,
            g.resources.memory_gb,
            g.resources.disk_floor(),
        )?;
    }

    let pid = if tart::is_running(name)? {
        println!("[{name}] VM already running");
        state_read(name)?.map(|s| s.pid).unwrap_or(0)
    } else {
        let log_path = logs_dir().join(format!("{name}.log"));
        let pid = tart::start(name, &log_path)?;
        println!("[{name}] started VM (tart pid {pid}), waiting for IP...");
        pid
    };

    let guest_ip = tart::ip(name, 120)?;
    println!("[{name}] guest IP {guest_ip}");

    // Source of truth for "is this runner healthy" is GitHub's runners API,
    // not a local state file — the state file is a cache and can lie if
    // provisioning failed halfway or the agent was uninstalled in-guest.
    let existing = github::find_runner(&cfg.github.org, pat, name)?;
    if matches!(&existing, Some(r) if r.status == "online") {
        println!("[{name}] runner online in GitHub; skipping provision");
        state_write(name, &build_state(g, name, pid, &guest_ip, golden_fp.as_deref()))?;
        return Ok(());
    }

    if let Some(r) = existing {
        // Deleting stale-but-offline registrations before re-registering
        // keeps the state machine tractable and prevents orphan rows.
        println!(
            "[{name}] pruning stale GitHub registration (id={}, status={:?})",
            r.id, r.status
        );
        if let Err(e) = github::delete_runner(&cfg.github.org, pat, r.id) {
            println!("[{name}] warn: could not prune stale registration: {e}");
        }
    }

    let token = github::registration_token(&cfg.github.org, pat)?;
    provision::provision(provision::ProvisionArgs {
        kind: g.kind.as_str(),
        host: &guest_ip,
        user: GUEST_USER,
        password: GUEST_PASS,
        org: &cfg.github.org,
        token: &token,
        name,
        labels: &g.labels,
        runner_version: &cfg.runner_version,
        scripts_dir: &scripts_dir(),
    })?;

    if !wait_for_online(&cfg.github.org, pat, name, HEALTHCHECK_TIMEOUT_S)? {
        bail!(
            "runner never reported online in GitHub within {HEALTHCHECK_TIMEOUT_S}s — \
             check the VM's ~/actions-runner/_diag/Runner_*.log"
        );
    }

    state_write(name, &build_state(g, name, pid, &guest_ip, golden_fp.as_deref()))?;
    println!("[{name}] runner registered and online");
    Ok(())
}

/// If the golden drifted since this VM was cloned, destroy it so the caller
/// can re-clone from the fresh image. No-op when the VM doesn't exist yet or
/// we have no stored fingerprint to compare against.
fn reclone_if_drifted(name: &str, image: &str, golden_fp: Option<&str>) -> Result<()> {
    if !tart::exists(name)? {
        return Ok(());
    }
    let Some(stored) = state_read(name)? else {
        return Ok(());
    };
    let (Some(golden), Some(stored_fp)) = (golden_fp, not_empty(&stored.golden_fingerprint))
    else {
        return Ok(());
    };
    if stored_fp == golden {
        return Ok(());
    }
    println!(
        "[{name}] golden {image:?} drifted since clone ({stored_fp} -> {golden}); \
         destroying and re-cloning"
    );
    tart::stop(name);
    tart::delete(name);
    let _ = fs::remove_file(state_file(name));
    Ok(())
}

fn not_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

fn build_state(
    g: &RunnerGroup,
    name: &str,
    pid: u32,
    ip: &str,
    golden_fp: Option<&str>,
) -> StateFile {
    StateFile {
        name: name.into(),
        vm: name.into(),
        pid,
        ip: ip.into(),
        kind: g.kind.as_str().into(),
        image: g.image.clone(),
        group: g.name.clone(),
        labels: g.labels.clone(),
        golden_fingerprint: golden_fp.unwrap_or("").into(),
        started_at: Utc::now().to_rfc3339(),
    }
}

/// Poll GitHub until the named runner reports `status=online`.
fn wait_for_online(org: &str, pat: &str, name: &str, timeout_s: u64) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let mut last_status = String::from("missing");
    while Instant::now() < deadline {
        last_status = match github::find_runner(org, pat, name)? {
            Some(r) if r.status == "online" => return Ok(true),
            Some(r) => r.status,
            None => "missing".into(),
        };
        sleep(Duration::from_secs(3));
    }
    println!("[{name}] runner last seen in GitHub as: {last_status}");
    Ok(false)
}

fn cmd_down(cfg: &FleetConfig, only: Option<HashSet<String>>, destroy: bool) -> Result<u8> {
    if !state_dir().exists() {
        println!("no runners tracked");
        return Ok(0);
    }

    // Still stop VMs even if we can't mint GitHub tokens.
    let pat = cfg.pat().ok();

    for state in tracked_state()? {
        let name = &state.name;
        if let Some(f) = &only {
            if !f.contains(name) && !f.contains(&state.group) {
                continue;
            }
        }

        println!("[{name}] tearing down...");

        if let (false, Some(pat)) = (state.ip.is_empty(), pat.as_deref()) {
            match github::removal_token(&cfg.github.org, pat) {
                Ok(token) => {
                    if let Err(e) = provision::remove(&state.ip, GUEST_USER, GUEST_PASS, &token) {
                        println!("[{name}] removal warning: {e}");
                    }
                }
                Err(e) => println!("[{name}] removal warning: {e}"),
            }
        }

        // Belt and suspenders: the in-guest `config.sh remove` is the
        // graceful path. If the VM is dead or misbehaving, fall back to the
        // orgs API — either way GitHub ends up clean.
        if let Some(pat) = pat.as_deref() {
            match github::find_runner(&cfg.github.org, pat, name) {
                Ok(Some(r)) => {
                    if let Err(e) = github::delete_runner(&cfg.github.org, pat, r.id) {
                        println!("[{name}] API prune warning: {e}");
                    }
                }
                Ok(None) => {}
                Err(e) => println!("[{name}] API prune warning: {e}"),
            }
        }

        tart::stop(name);
        if destroy {
            tart::delete(name);
            println!("[{name}] VM destroyed");
        }
        let _ = fs::remove_file(state_file(name));
        println!("[{name}] stopped");
    }
    Ok(0)
}

/// Destroy each target VM and re-create it from its golden. Resets
/// `_work/`, tool caches, and any in-VM cruft accumulated since the last
/// recycle — the bulk-disk-cleanup answer that pairs with the per-job hook.
/// Also evicts stale tart OCI/IPSW cache entries when the config opts in.
fn cmd_recycle(cfg: &FleetConfig, only: Option<HashSet<String>>, force: bool) -> Result<u8> {
    let rc = cmd_down(cfg, only.clone(), true)?;
    if rc != 0 {
        return Ok(rc);
    }
    // Prune while the VMs are down: the freed space is available before the
    // re-cloned runners start growing again. Non-fatal — a failed prune
    // shouldn't leave the fleet offline.
    if let Some(days) = cfg.host.cache_retention_days {
        println!("[host] pruning tart caches not accessed in {days} days");
        if let Err(e) = tart::prune_caches(days) {
            println!("[host] cache prune warning: {e}");
        }
    }
    cmd_up(cfg, only, force)
}

fn cmd_status() -> Result<u8> {
    let tracked = tracked_state_if_any()?;
    if tracked.is_empty() {
        println!("no runners tracked");
        return Ok(0);
    }
    for s in tracked {
        let running = tart::is_running(&s.name).unwrap_or(false);
        let ip = if s.ip.is_empty() { "?".into() } else { s.ip };
        println!(
            "{name:<22} group={group:<14} kind={kind:<5} ip={ip:<15} running={running}",
            name = s.name,
            group = s.group,
            kind = s.kind,
            running = if running { "yes" } else { "no" },
        );
    }
    Ok(0)
}

fn cmd_build(image: Option<&str>, as_name: Option<&str>) -> Result<u8> {
    let dir = scripts_dir();
    let mut available: Vec<String> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("bake-")
                .and_then(|s| s.strip_suffix(".sh"))
                .map(str::to_string)
        })
        .collect();
    available.sort();

    let Some(image) = image else {
        if available.is_empty() {
            eprintln!("no bake scripts found in scripts/");
            return Ok(1);
        }
        println!("Available goldens:");
        for n in &available {
            println!("  - {n}");
        }
        println!("\nRun: bakery build <name>");
        return Ok(0);
    };

    let script = dir.join(format!("bake-{image}.sh"));
    if !script.exists() {
        eprintln!("no bake script for {image:?}");
        if !available.is_empty() {
            eprintln!("available: {}", available.join(", "));
        }
        return Ok(1);
    }

    let dest = as_name.unwrap_or(image);
    println!(
        ">>> building golden {dest:?} via {}",
        script.file_name().and_then(|s| s.to_str()).unwrap_or("")
    );
    let status = Command::new(&script)
        .arg(dest)
        .status()
        .with_context(|| format!("spawning {}", script.display()))?;
    Ok(status.code().unwrap_or(1).try_into().unwrap_or(1))
}

#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    name: String,
    vm: String,
    pid: u32,
    ip: String,
    kind: String,
    image: String,
    group: String,
    labels: Vec<String>,
    // Added after the initial schema; tolerate state files written before
    // drift-detection existed so `down`/`status` don't silently skip them.
    #[serde(default)]
    golden_fingerprint: String,
    started_at: String,
}

fn state_file(name: &str) -> PathBuf {
    state_dir().join(format!("{name}.json"))
}

fn state_read(name: &str) -> Result<Option<StateFile>> {
    let path = state_file(name);
    if !path.exists() {
        return Ok(None);
    }
    let txt = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let state: StateFile =
        serde_json::from_str(&txt).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(state))
}

fn state_write(name: &str, s: &StateFile) -> Result<()> {
    fs::create_dir_all(state_dir())?;
    let path = state_file(name);
    let txt = serde_json::to_string_pretty(s)?;
    fs::write(&path, txt).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn tracked_state() -> Result<Vec<StateFile>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(state_dir())
        .context("reading state dir")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    // Surface read/parse failures instead of silently skipping — a state file
    // that won't parse is almost always a schema drift bug, and silently
    // dropping it makes `down` appear to succeed while leaving VMs running.
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        match fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(
            |s| -> Result<StateFile> { serde_json::from_str(&s).map_err(Into::into) },
        ) {
            Ok(s) => out.push(s),
            Err(e) => eprintln!("warn: skipping {}: {e:#}", p.display()),
        }
    }
    Ok(out)
}

fn tracked_state_if_any() -> Result<Vec<StateFile>> {
    if !state_dir().exists() {
        return Ok(vec![]);
    }
    tracked_state()
}
