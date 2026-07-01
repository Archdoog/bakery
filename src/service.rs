//! Install a launchd LaunchAgent that keeps the fleet online on a daily
//! driver: `bakery up` re-runs at login, on screen unlock, and on a fixed
//! interval (launchd coalesces intervals missed during sleep into one run at
//! wake, so the fleet phones back in within seconds of the lid opening).

use std::env;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub const LABEL: &str = "com.bakery.up";

/// PATH for the agent: launchd gives agents a bare-bones PATH that lacks
/// Homebrew, where `tart` and `sshpass` live.
const AGENT_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Secrets file sourced by the agent before exec'ing `bakery up`. Lives under
/// state/ (gitignored) with 0600 perms so the PAT never lands in the plist.
const ENV_FILE: &str = "state/service.env";

const SERVICE_LOG: &str = "logs/service.log";

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn gui_target() -> String {
    // Safety: getuid has no failure modes or preconditions.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Shell fragment the agent runs. Sources the env file for the PAT, then
/// retries `up` a few times — right after wake the network is often not up
/// yet, so the first GitHub call can fail before Wi-Fi reassociates.
fn agent_script(root: &Path, config: &Path) -> String {
    format!(
        r#"cd '{root}' || exit 1
if [ -f {env_file} ]; then set -a; . ./{env_file}; set +a; fi
n=0
until '{exe}' -c '{config}' up; do
  n=$((n+1)); [ "$n" -ge 3 ] && exit 1
  sleep 15
done"#,
        root = root.display(),
        env_file = ENV_FILE,
        exe = current_exe_display(),
        config = config.display(),
    )
}

fn current_exe_display() -> String {
    env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "bakery".into())
}

fn render_plist(root: &Path, config: &Path, interval_s: u64) -> String {
    let log = root.join(SERVICE_LOG);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>{script}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{AGENT_PATH}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>StartInterval</key>
  <integer>{interval_s}</integer>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>LaunchEvents</key>
  <dict>
    <key>com.apple.notifyd.matching</key>
    <dict>
      <key>ScreenUnlocked</key>
      <dict>
        <key>Notification</key>
        <string>com.apple.screenIsUnlocked</string>
      </dict>
    </dict>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        script = xml_escape(&agent_script(root, config)),
        log = xml_escape(&log.display().to_string()),
    )
}

fn write_secret(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))?;
    Ok(())
}

/// Seed state/service.env from the caller's environment so the agent gets the
/// PAT without it ever entering the plist.
fn ensure_env_file(root: &Path, token_env: &str) -> Result<()> {
    let path = root.join(ENV_FILE);
    match env::var(token_env) {
        Ok(val) => {
            write_secret(&path, &format!("{token_env}='{val}'\n"))?;
            println!("wrote {ENV_FILE} from ${token_env} (chmod 600)");
        }
        Err(_) if path.exists() => {
            println!("${token_env} not set in this shell; keeping existing {ENV_FILE}");
        }
        Err(_) => {
            write_secret(
                &path,
                &format!(
                    "# Sourced by the {LABEL} LaunchAgent before `bakery up`.\n{token_env}=''\n"
                ),
            )?;
            println!(
                "WARNING: ${token_env} not set — wrote a stub {ENV_FILE}; \
                 fill in the PAT or the agent's `up` runs will fail"
            );
        }
    }
    Ok(())
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("spawning launchctl")
}

pub fn install(config: &Path, token_env: &str, interval_s: u64) -> Result<u8> {
    let root = env::current_dir().context("resolving repo root")?;
    fs::create_dir_all(root.join("state")).context("creating state dir")?;
    fs::create_dir_all(root.join("logs")).context("creating logs dir")?;
    ensure_env_file(&root, token_env)?;

    let plist = plist_path()?;
    if let Some(dir) = plist.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    fs::write(&plist, render_plist(&root, config, interval_s))
        .with_context(|| format!("writing {}", plist.display()))?;

    let target = gui_target();
    // Unload any previous version first; failure just means it wasn't loaded.
    let _ = launchctl(&["bootout", &format!("{target}/{LABEL}")]);
    let out = launchctl(&["bootstrap", &target, &plist.display().to_string()])?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    println!("installed {}", plist.display());
    println!("agent runs `bakery up` at login, on screen unlock, and every {interval_s}s (missed intervals fire on wake)");
    println!("`up` output: {}", root.join(SERVICE_LOG).display());
    println!("note: the plist points at this binary — re-run `bakery service install` after `cargo install`");
    Ok(0)
}

pub fn uninstall() -> Result<u8> {
    let plist = plist_path()?;
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", gui_target())]);
    if plist.exists() {
        fs::remove_file(&plist).with_context(|| format!("removing {}", plist.display()))?;
        println!("removed {}", plist.display());
    } else {
        println!("no agent installed ({} not found)", plist.display());
    }
    // The env file holds a PAT copy; keep the host clean once the agent is gone.
    let env_file = env::current_dir()?.join(ENV_FILE);
    if env_file.exists() {
        fs::remove_file(&env_file).with_context(|| format!("removing {}", env_file.display()))?;
        println!("removed {ENV_FILE}");
    }
    Ok(0)
}

pub fn status() -> Result<u8> {
    let plist = plist_path()?;
    let out = launchctl(&["print", &format!("{}/{LABEL}", gui_target())])?;
    if !out.status.success() {
        println!("not loaded ({LABEL} missing from {})", gui_target());
        println!(
            "plist {}: {}",
            plist.display(),
            if plist.exists() {
                "present — load with `bakery service install`"
            } else {
                "absent"
            }
        );
        return Ok(1);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    println!("loaded as {}/{LABEL}", gui_target());
    // `launchctl print` nests sub-sections (event channels) that repeat these
    // keys; only the first occurrence describes the job itself.
    for key in ["state =", "runs =", "last exit code ="] {
        if let Some(t) = text.lines().map(str::trim).find(|t| t.starts_with(key)) {
            println!("  {t}");
        }
    }

    let log = env::current_dir()?.join(SERVICE_LOG);
    if let Some(tail) = tail_bytes(&log, 2048)? {
        println!("--- {} (tail) ---", log.display());
        for line in tail
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            println!("  {line}");
        }
    }
    Ok(0)
}

/// Last `max` bytes of `path` as lossy UTF-8, or None when absent/empty.
fn tail_bytes(path: &Path, max: u64) -> Result<Option<String>> {
    let Ok(mut f) = fs::File::open(path) else {
        return Ok(None);
    };
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(None);
    }
    f.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_embeds_label_trigger_and_interval() {
        let p = render_plist(Path::new("/tmp/repo"), Path::new("runners.yaml"), 300);
        assert!(p.contains("<string>com.bakery.up</string>"));
        assert!(p.contains("com.apple.screenIsUnlocked"));
        assert!(p.contains("<integer>300</integer>"));
        assert!(p.contains("cd '/tmp/repo' || exit 1"));
        assert!(p.contains("-c 'runners.yaml' up"));
        assert!(p.contains("/tmp/repo/logs/service.log"));
    }

    #[test]
    fn xml_escape_covers_plist_metachars() {
        assert_eq!(xml_escape("a & b <c> d"), "a &amp; b &lt;c&gt; d");
    }
}
