#!/bin/bash
# Installs, configures, and starts a GitHub Actions runner inside a macOS guest.
# Run via SSH from the host. All args are required.
# Args: ORG REGISTRATION_TOKEN NAME LABELS_CSV RUNNER_VERSION
set -euo pipefail

ORG="$1"
TOKEN="$2"
NAME="$3"
LABELS="$4"
VERSION="$5"

RUNNER_DIR="$HOME/actions-runner"
mkdir -p "$RUNNER_DIR"
cd "$RUNNER_DIR"

# Keep the guest clock tight against NTP — must happen before config.sh hits
# the network. Tart/VZ guests drift while the host sleeps or the VM is paused;
# on resume the clock can sit minutes-to-hours behind real time. The runner
# mints its OAuth token stamped with the *local* clock, so a behind-clock guest
# presents a token GitHub's vstoken endpoint sees as already-expired ->
# HTTP BadRequest, the broker session never opens, the listener crash-loops,
# and GitHub eventually deletes the registration ("not connected recently").
# macOS "Network Time: On" alone does NOT re-sync promptly across a VM resume
# (we have observed ~3h skew with it enabled), so we (a) force a step-sync now,
# before registration, and (b) install a LaunchDaemon that re-steps every 5 min
# to bound drift between jobs. Override the server via BAKERY_NTP_SERVER.
NTP_SERVER="${BAKERY_NTP_SERVER:-time.apple.com}"
sudo systemsetup -setnetworktimeserver "$NTP_SERVER" >/dev/null 2>&1 || true
sudo systemsetup -setusingnetworktime on             >/dev/null 2>&1 || true
sudo /usr/bin/sntp -sS "$NTP_SERVER"                 >/dev/null 2>&1 || true

sudo tee /Library/LaunchDaemons/com.bakery.timesync.plist >/dev/null <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.bakery.timesync</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/sntp</string>
    <string>-sS</string>
    <string>${NTP_SERVER}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>StartInterval</key>
  <integer>300</integer>
</dict>
</plist>
PLIST
sudo launchctl bootout   system /Library/LaunchDaemons/com.bakery.timesync.plist 2>/dev/null || true
sudo launchctl bootstrap system /Library/LaunchDaemons/com.bakery.timesync.plist 2>/dev/null || true

if [[ ! -x ./config.sh ]]; then
  curl -fsSL -o runner.tar.gz \
    "https://github.com/actions/runner/releases/download/v${VERSION}/actions-runner-osx-arm64-${VERSION}.tar.gz"
  tar xzf runner.tar.gz
  rm runner.tar.gz
fi

# Stop any prior listener/service so --replace can reconfigure cleanly.
./svc.sh stop 2>/dev/null || true
./svc.sh uninstall 2>/dev/null || true
pkill -f Runner.Listener || true
sleep 1

# config.sh refuses if any .runner* or .credentials* file is present. The
# runner auto-updater renames these on upgrade (.runner_migrated,
# .credentials_rsaparams_migrated, etc.) and each new variant trips the same
# "already configured" check. Glob-based cleanup is future-proof against any
# further renames the updater invents.
rm -f .runner* .credentials*

./config.sh \
  --url    "https://github.com/${ORG}" \
  --token  "${TOKEN}" \
  --name   "${NAME}" \
  --labels "${LABELS}" \
  --unattended --replace

# actions-runner runs $ACTIONS_RUNNER_HOOK_JOB_COMPLETED after every job. We
# use it to evict the parts of the workspace that bloat fastest: GHA's own
# _temp scratch, stale _actions/_diag files, and Xcode caches that regenerate
# cheaply. DerivedData is reaped in every known location — not just Xcode's
# default, since projects routinely point `-derivedDataPath` elsewhere — and a
# disk-pressure floor escalates to an aggressive sweep when a runner fills
# mid-week. Tool caches (Homebrew, ~/Library/Caches/CocoaPods, ~/.gradle/caches)
# are deliberately untouched — those are what make self-hosted faster than
# macos-latest. Simulator devices are also left alone: tart clones preserve
# mtimes, so a >7d-old golden's bake-created devices look identical to test
# detritus to `find -mtime`, and reaping them broke iOS CI. Whole-VM resets
# come from `bakery recycle`.
HOOK_DIR="$RUNNER_DIR/runner-hooks"
mkdir -p "$HOOK_DIR"
cat >"$HOOK_DIR/job-completed.sh" <<'HOOK'
#!/bin/bash
# Conservative per-job cleanup with a disk-pressure safety net. Errors are
# non-fatal — the runner agent treats a non-zero hook exit as a job failure,
# which we never want here.
set -u
RUNNER_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$RUNNER_ROOT/_work"

rm -rf "$WORK/_temp"/* 2>/dev/null || true
find "$WORK/_actions" -mindepth 1 -maxdepth 3 -type d -mtime +7 \
    -exec rm -rf {} + 2>/dev/null || true
find "$RUNNER_ROOT/_diag" -type f -mtime +7 -delete 2>/dev/null || true

# DerivedData defaults to Xcode's location, but projects routinely point
# `xcodebuild -derivedDataPath` elsewhere (~/.derived-data is a common choice,
# and some keep it inside the checkout). A path the hook doesn't know about
# grows unbounded until the disk fills mid-week. Reap across every known root;
# operators can name extra roots via RUNNER_DERIVED_DATA_DIRS (colon-separated)
# in the runner's .env.
DD_DIRS=(
  "$HOME/Library/Developer/Xcode/DerivedData"
  "$HOME/.derived-data"
)
if [ -n "${RUNNER_DERIVED_DATA_DIRS:-}" ]; then
  IFS=':' read -r -a _extra <<< "$RUNNER_DERIVED_DATA_DIRS"
  DD_DIRS+=("${_extra[@]}")
fi

# Xcode ModuleCache regenerates from headers on next build — safe to drop.
# DerivedData entries older than a week are almost always for branches the
# runner won't see again; keep recent ones for incremental build wins.
for DD in "${DD_DIRS[@]}"; do
  [ -n "$DD" ] && [ -d "$DD" ] || continue
  rm -rf "$DD/ModuleCache.noindex" 2>/dev/null || true
  find "$DD" -mindepth 1 -maxdepth 1 -type d -mtime +7 \
      -exec rm -rf {} + 2>/dev/null || true
done

# Safety net: age-based reaping keeps caches warm but can't react to a runner
# that fills mid-week (a fat custom -derivedDataPath, a runaway artifact). If
# free space falls below the floor, escalate — drop ALL DerivedData regardless
# of age, plus any DerivedData dirs living inside the checkout. A cold rebuild
# is far cheaper than "No space left on device" failing every later job. Tune
# the floor with RUNNER_DISK_MIN_FREE_GB (default 15).
MIN_FREE_GB="${RUNNER_DISK_MIN_FREE_GB:-15}"
FREE_GB="$(df -g / 2>/dev/null | awk 'NR==2 {print $4}')"
if [ -n "$FREE_GB" ] && [ "$FREE_GB" -lt "$MIN_FREE_GB" ]; then
  echo "[hook] free ${FREE_GB}G < ${MIN_FREE_GB}G floor — aggressive DerivedData sweep"
  for DD in "${DD_DIRS[@]}"; do
    [ -n "$DD" ] && [ -d "$DD" ] || continue
    find "$DD" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true
  done
  find "$WORK" -type d -name DerivedData -prune -exec rm -rf {} + 2>/dev/null || true
fi

df -h / 2>/dev/null | awk 'NR==2 {printf "[hook] disk: %s used of %s (%s)\n",$3,$2,$5}'
exit 0
HOOK
chmod +x "$HOOK_DIR/job-completed.sh"

# Runner jobs run in a non-interactive non-login shell, so ~/.zshrc is never
# sourced. actions-runner reads <runner-dir>/.env into every job's env.
{
  NEW_PATH="/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
  ANDROID_ROOT="$HOME/Library/Android/sdk"
  if [[ -d "$ANDROID_ROOT" ]]; then
    echo "ANDROID_HOME=$ANDROID_ROOT"
    echo "ANDROID_SDK_ROOT=$ANDROID_ROOT"
    NEW_PATH="$NEW_PATH:$ANDROID_ROOT/cmdline-tools/latest/bin:$ANDROID_ROOT/platform-tools:$ANDROID_ROOT/emulator"
    # cargo-ndk and AGP both look up the NDK via ANDROID_NDK_HOME. Pick the
    # newest installed NDK so the env stays valid if bake bumps the version.
    if [[ -d "$ANDROID_ROOT/ndk" ]]; then
      NDK_VER="$(ls -1 "$ANDROID_ROOT/ndk" | sort -V | tail -1)"
      if [[ -n "$NDK_VER" ]]; then
        echo "ANDROID_NDK_HOME=$ANDROID_ROOT/ndk/$NDK_VER"
        echo "ANDROID_NDK_ROOT=$ANDROID_ROOT/ndk/$NDK_VER"
      fi
    fi
  fi
  if JH="$(/usr/libexec/java_home -v 21 2>/dev/null)"; then
    echo "JAVA_HOME=$JH"
    NEW_PATH="$JH/bin:$NEW_PATH"
  fi
  # rustup binaries (rustc, cargo, cargo-ndk) live under ~/.cargo/bin.
  if [[ -d "$HOME/.cargo/bin" ]]; then
    NEW_PATH="$HOME/.cargo/bin:$NEW_PATH"
  fi
  echo "PATH=$NEW_PATH"
  echo "ACTIONS_RUNNER_HOOK_JOB_COMPLETED=$HOOK_DIR/job-completed.sh"
} > .env

# Install as a launchd LaunchAgent so auto-updates and reboots don't kill it.
./svc.sh install
./svc.sh start
echo "actions-runner started as launchd service"
