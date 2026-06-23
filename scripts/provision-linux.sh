#!/bin/bash
# Installs, configures, and starts a GitHub Actions runner inside a Linux guest.
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
    "https://github.com/actions/runner/releases/download/v${VERSION}/actions-runner-linux-arm64-${VERSION}.tar.gz"
  tar xzf runner.tar.gz
  rm runner.tar.gz

  if [[ -x ./bin/installdependencies.sh ]]; then
    sudo ./bin/installdependencies.sh || true
  fi
fi

sudo ./svc.sh stop 2>/dev/null || true
sudo ./svc.sh uninstall 2>/dev/null || true
pkill -f Runner.Listener || true
sleep 1

# config.sh refuses if the runner is still configured locally; force-clean.
rm -f .runner .credentials .credentials_rsaparams

./config.sh \
  --url    "https://github.com/${ORG}" \
  --token  "${TOKEN}" \
  --name   "${NAME}" \
  --labels "${LABELS}" \
  --unattended --replace

# actions-runner runs $ACTIONS_RUNNER_HOOK_JOB_COMPLETED after every job. We
# use it to evict the parts of the workspace that bloat fastest: GHA's own
# _temp scratch and stale _actions/_diag files. Tool caches (~/.cargo,
# ~/.gradle, /opt/hostedtoolcache, npm/pnpm stores) are deliberately
# untouched — those are what make self-hosted faster than ubuntu-latest.
# Whole-VM resets come from `bakery recycle`.
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

df -h / 2>/dev/null | awk 'NR==2 {printf "[hook] disk: %s used of %s (%s)\n",$3,$2,$5}'
exit 0
HOOK
chmod +x "$HOOK_DIR/job-completed.sh"

# Runner jobs run in a non-interactive non-login shell, so ~/.bashrc is never
# sourced. actions-runner reads <runner-dir>/.env into every job's env.
{
  NEW_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin"
  # corepack prompts y/n before downloading a packageManager; non-interactive
  # CI shells never answer and the job hangs until GHA timeout. Force proceed.
  echo "COREPACK_ENABLE_DOWNLOAD_PROMPT=0"
  if [[ -d /opt/android-sdk ]]; then
    echo "ANDROID_HOME=/opt/android-sdk"
    echo "ANDROID_SDK_ROOT=/opt/android-sdk"
    NEW_PATH="$NEW_PATH:/opt/android-sdk/cmdline-tools/latest/bin:/opt/android-sdk/platform-tools"
  fi
  [[ -d "$HOME/.cargo/bin" ]]  && NEW_PATH="$NEW_PATH:$HOME/.cargo/bin"
  [[ -d "$HOME/.local/bin" ]]  && NEW_PATH="$NEW_PATH:$HOME/.local/bin"
  echo "PATH=$NEW_PATH"
  echo "ACTIONS_RUNNER_HOOK_JOB_COMPLETED=$HOOK_DIR/job-completed.sh"
} > .env

# Install as a systemd service so auto-updates and reboots don't kill it.
sudo ./svc.sh install "$USER"
sudo ./svc.sh start
echo "actions-runner started as systemd service"
