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
# cheaply. Tool caches (Homebrew, ~/Library/Caches/CocoaPods,
# ~/.gradle/caches) are deliberately untouched — those are what make
# self-hosted faster than macos-latest. Simulator devices are also left
# alone: tart clones preserve mtimes, so a >7d-old golden's bake-created
# devices look identical to test detritus to `find -mtime`, and reaping
# them broke iOS CI. Whole-VM resets come from `bakery recycle`.
HOOK_DIR="$RUNNER_DIR/runner-hooks"
mkdir -p "$HOOK_DIR"
cat >"$HOOK_DIR/job-completed.sh" <<'HOOK'
#!/bin/bash
# Conservative per-job cleanup. Errors are non-fatal — the runner agent
# treats a non-zero hook exit as a job failure, which we never want here.
set -u
RUNNER_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$RUNNER_ROOT/_work"

rm -rf "$WORK/_temp"/* 2>/dev/null || true
find "$WORK/_actions" -mindepth 1 -maxdepth 3 -type d -mtime +7 \
    -exec rm -rf {} + 2>/dev/null || true
find "$RUNNER_ROOT/_diag" -type f -mtime +7 -delete 2>/dev/null || true

# Xcode ModuleCache regenerates from headers on next build — safe to drop.
# DerivedData entries older than a week are almost always for branches the
# runner won't see again; keep recent ones for incremental build wins.
DD="$HOME/Library/Developer/Xcode/DerivedData"
rm -rf "$DD/ModuleCache.noindex" 2>/dev/null || true
find "$DD" -mindepth 1 -maxdepth 1 -type d -mtime +7 \
    -exec rm -rf {} + 2>/dev/null || true

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
