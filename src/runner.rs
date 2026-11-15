//! Runner provisioning and replacement.

use crate::{
    github,
    models::{FleetConfig, RunnerGroup},
    paths::{logs_dir, scripts_dir},
    provision,
    state::{state_file, state_read, state_write, StateFile},
    tart,
};
use anyhow::{bail, Result};
use chrono::Utc;
use std::{
    fs,
    thread::sleep,
    time::{Duration, Instant},
};

pub(crate) const GUEST_USER: &str = "admin";
pub(crate) const GUEST_PASS: &str = "admin";
const HEALTHCHECK_TIMEOUT_S: u64 = 90;

#[derive(Debug, PartialEq, Eq)]
enum EphemeralAction {
    Keep,
    Replace,
}

fn ephemeral_action(
    runner: Option<&github::Runner>,
    stored: Option<bool>,
) -> Result<EphemeralAction> {
    match (runner, stored) {
        (None, _) => Ok(EphemeralAction::Replace),
        (Some(_), Some(true)) => Ok(EphemeralAction::Keep),
        (Some(_), _) => bail!("An existing runner does not have tracked ephemeral mode. Recycle it when idle before you enable ephemeral mode."),
    }
}

/// Start or reconcile runner `name` from group `g` in `cfg`, with GitHub credentials `pat`.
///
/// # Errors
/// Returns an error if registration, VM operations, provisioning, or state access fails.
pub(crate) fn bring_up(cfg: &FleetConfig, g: &RunnerGroup, name: &str, pat: &str) -> Result<()> {
    println!("[{name}] bringing up...");

    if g.ephemeral {
        let existing = github::find_runner(&cfg.github.org, pat, name)?;
        let stored = state_read(name)?;
        match ephemeral_action(existing.as_ref(), stored.as_ref().map(|s| s.ephemeral))? {
            EphemeralAction::Keep => return Ok(()),
            EphemeralAction::Replace => {
                if tart::exists(name)? {
                    if let Some(state) = stored.filter(|s| s.ephemeral) {
                        let archive = logs_dir()
                            .join(format!("{name}-{}.tar.gz", Utc::now().timestamp_millis()));
                        provision::archive_logs(&state.ip, GUEST_USER, GUEST_PASS, &archive)?;
                        println!("[{name}] diagnostics saved to {}", archive.display());
                    }
                    tart::stop(name);
                    tart::try_delete(name)?;
                }
                if state_file(name).exists() {
                    fs::remove_file(state_file(name))?;
                }
            }
        }
    } else if state_read(name)?.is_some_and(|s| s.ephemeral) {
        bail!("Runner {name} uses ephemeral mode. Stop the watcher and recycle this runner before you change its mode.");
    }

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

    // Saved state does not prove that the runner is online.
    let existing = github::find_runner(&cfg.github.org, pat, name)?;
    if matches!(&existing, Some(r) if r.status == "online") {
        println!("[{name}] runner online in GitHub; skipping provision");
        state_write(
            name,
            &build_state(g, name, pid, &guest_ip, golden_fp.as_deref()),
        )?;
        return Ok(());
    }

    if let Some(r) = existing {
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
        ephemeral: g.ephemeral,
        runner_version: &cfg.runner_version,
        scripts_dir: &scripts_dir(),
    })?;

    state_write(
        name,
        &build_state(g, name, pid, &guest_ip, golden_fp.as_deref()),
    )?;
    if g.ephemeral {
        println!("[{name}] ephemeral runner service started");
        return Ok(());
    }

    if !wait_for_online(&cfg.github.org, pat, name, HEALTHCHECK_TIMEOUT_S)? {
        bail!(
            "runner never reported online in GitHub within {HEALTHCHECK_TIMEOUT_S}s — \
             check the VM's ~/actions-runner/_diag/Runner_*.log"
        );
    }

    state_write(
        name,
        &build_state(g, name, pid, &guest_ip, golden_fp.as_deref()),
    )?;
    println!("[{name}] runner registered and online");
    Ok(())
}

fn reclone_if_drifted(name: &str, image: &str, golden_fp: Option<&str>) -> Result<()> {
    if !tart::exists(name)? {
        return Ok(());
    }
    let Some(stored) = state_read(name)? else {
        return Ok(());
    };
    let (Some(golden), Some(stored_fp)) = (golden_fp, not_empty(&stored.golden_fingerprint)) else {
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
        ephemeral: g.ephemeral,
        golden_fingerprint: golden_fp.unwrap_or("").into(),
        started_at: Utc::now().to_rfc3339(),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(status: &str) -> github::Runner {
        github::Runner {
            id: 1,
            name: "ubuntu-1".into(),
            status: status.into(),
        }
    }

    #[test]
    fn registered_ephemeral_runners_are_never_replaced() {
        for status in ["online", "offline", "unknown"] {
            assert_eq!(
                ephemeral_action(Some(&runner(status)), Some(true)).unwrap(),
                EphemeralAction::Keep
            );
        }
    }

    #[test]
    fn missing_registrations_require_a_fresh_vm() {
        for stored in [None, Some(false), Some(true)] {
            assert_eq!(
                ephemeral_action(None, stored).unwrap(),
                EphemeralAction::Replace
            );
        }
    }

    #[test]
    fn existing_persistent_or_untracked_runners_require_manual_conversion() {
        for stored in [None, Some(false)] {
            for status in ["online", "offline"] {
                assert!(ephemeral_action(Some(&runner(status)), stored).is_err());
            }
        }
    }
}
