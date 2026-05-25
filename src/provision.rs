//! SSH into a guest and run the appropriate provision script.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::thread::sleep;
use std::time::{Duration, Instant};

use thiserror::Error;

/// Common SSH options: no host-key prompts, short connect timeout, and force
/// password auth so sshd doesn't count agent keys against MaxAuthTries.
const SSH_OPTS: &[&str] = &[
    "-o", "StrictHostKeyChecking=no",
    "-o", "UserKnownHostsFile=/dev/null",
    "-o", "LogLevel=ERROR",
    "-o", "ConnectTimeout=15",
    "-o", "PubkeyAuthentication=no",
    "-o", "PreferredAuthentications=password",
    "-o", "IdentitiesOnly=yes",
];

#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("sshpass not found on PATH. Install with: brew install sshpass")]
    SshpassMissing,
    #[error("provision script missing: {0}")]
    ScriptMissing(PathBuf),
    #[error("spawning {cmd}")]
    Spawn {
        cmd: String,
        #[source]
        source: io::Error,
    },
    #[error("ssh to {host} exited {status}")]
    SshFailed { host: String, status: ExitStatus },
    #[error("scp {src} -> {dest} exited {status}")]
    ScpFailed {
        src: PathBuf,
        dest: String,
        status: ExitStatus,
    },
    #[error("SSH to {host} never came up after {after_s}s")]
    Timeout { host: String, after_s: u64 },
}

pub type Result<T> = std::result::Result<T, ProvisionError>;

pub fn ensure_sshpass() -> Result<()> {
    which("sshpass").ok_or(ProvisionError::SshpassMissing).map(drop)
}

fn which(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
}

/// Poll SSH until the guest accepts a connection. `tart ip` returns as soon
/// as DHCP hands out the address, which is well before sshd is up.
pub fn wait_for_ssh(host: &str, user: &str, password: &str, timeout_s: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    while Instant::now() < deadline {
        let ok = Command::new("sshpass")
            .args(["-p", password, "ssh"])
            .args(SSH_OPTS)
            .arg(format!("{user}@{host}"))
            .arg("true")
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Ok(());
        }
        sleep(Duration::from_secs(2));
    }
    Err(ProvisionError::Timeout {
        host: host.into(),
        after_s: timeout_s,
    })
}

pub struct ProvisionArgs<'a> {
    pub kind: &'a str,
    pub host: &'a str,
    pub user: &'a str,
    pub password: &'a str,
    pub org: &'a str,
    pub token: &'a str,
    pub name: &'a str,
    pub labels: &'a [String],
    pub runner_version: &'a str,
    pub scripts_dir: &'a Path,
}

pub fn provision(args: ProvisionArgs<'_>) -> Result<()> {
    ensure_sshpass()?;
    let script = args.scripts_dir.join(format!("provision-{}.sh", args.kind));
    if !script.exists() {
        return Err(ProvisionError::ScriptMissing(script));
    }

    wait_for_ssh(args.host, args.user, args.password, 180)?;

    let remote_path = format!("/tmp/provision-{}.sh", args.kind);
    scp(
        args.password,
        &script,
        &format!("{}@{}:{}", args.user, args.host, remote_path),
    )?;

    let labels_csv = args.labels.join(",");
    let remote_cmd = format!(
        "chmod +x {remote} && {remote} {org} {token} {name} {labels} {ver}",
        remote = remote_path,
        org = shell_quote(args.org),
        token = shell_quote(args.token),
        name = shell_quote(args.name),
        labels = shell_quote(&labels_csv),
        ver = shell_quote(args.runner_version),
    );
    ssh_run(
        args.password,
        &format!("{}@{}", args.user, args.host),
        &remote_cmd,
    )
}

/// Ask the in-guest runner to de-register itself. Best-effort.
///
/// Order matters: the runner is installed as a launchd service via
/// `svc.sh install` during `bakery up`, and `config.sh remove` refuses
/// to run while that service still exists ("Uninstall service first").
/// Each step is `|| true` because de-registration is best-effort — a
/// subsequent `bakery up` still handles stale registrations via
/// `config.sh --replace`.
pub fn remove(host: &str, user: &str, password: &str, token: &str) -> Result<()> {
    ensure_sshpass()?;
    let cmd = format!(
        "cd ~/actions-runner 2>/dev/null && \
         sudo ./svc.sh stop 2>/dev/null || true; \
         sudo ./svc.sh uninstall 2>/dev/null || true; \
         pkill -f Runner.Listener || true; \
         ./config.sh remove --token {} || true",
        shell_quote(token)
    );
    let _ = Command::new("sshpass")
        .args(["-p", password, "ssh"])
        .args(SSH_OPTS)
        .arg(format!("{user}@{host}"))
        .arg(&cmd)
        .status();
    Ok(())
}

fn ssh_run(password: &str, dest: &str, cmd: &str) -> Result<()> {
    let status = Command::new("sshpass")
        .args(["-p", password, "ssh"])
        .args(SSH_OPTS)
        .arg(dest)
        .arg(cmd)
        .status()
        .map_err(|source| ProvisionError::Spawn {
            cmd: "ssh".into(),
            source,
        })?;
    if !status.success() {
        return Err(ProvisionError::SshFailed {
            host: dest.into(),
            status,
        });
    }
    Ok(())
}

fn scp(password: &str, src: &Path, dest: &str) -> Result<()> {
    let status = Command::new("sshpass")
        .args(["-p", password, "scp"])
        .args(SSH_OPTS)
        .arg(src)
        .arg(dest)
        .status()
        .map_err(|source| ProvisionError::Spawn {
            cmd: "scp".into(),
            source,
        })?;
    if !status.success() {
        return Err(ProvisionError::ScpFailed {
            src: src.to_path_buf(),
            dest: dest.into(),
            status,
        });
    }
    Ok(())
}

/// POSIX shell single-quote: wrap in `'…'` and escape embedded single
/// quotes. Matches the semantics of Python's `shlex.quote`.
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(is_shell_safe) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\"'\"'");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn is_shell_safe(b: u8) -> bool {
    matches!(
        b,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'
            | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-' | b'_'
    )
}
