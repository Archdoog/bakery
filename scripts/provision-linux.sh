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

# --- reliable egress behind vmnet NAT ---
# Tart guests sit behind macOS vmnet's NAT. Two properties of that path silently
# truncate large TLS responses, which surface to callers as "Premature close"
# rather than a clean error:
#   - virtio segmentation offloads (tso/gso/gro/lro) hand oversized segments to a
#     NIC that mis-handles them under NAT, and
#   - the effective path MTU can be below the guest's default 1500 (e.g. when the
#     host is on a VPN), with PMTU discovery black-holed.
# Small requests/replies pass, so plain curl looks fine while real work breaks —
# e.g. firebase-tools' OAuth token POST to www.googleapis.com fails to refresh
# and `firebase deploy` aborts with "Failed to authenticate". A boot-time oneshot
# (also run now) disables the offloads and clamps MTU on the default-route
# interface, named generically so it survives enpXsY renames across hosts.
# Installed here (not only in the golden) so existing goldens pick it up on the
# next `bakery` provision without a full re-bake.
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ethtool >/dev/null 2>&1 || true
sudo tee /usr/local/sbin/nic-tune.sh >/dev/null <<'NICTUNE'
#!/bin/bash
set -u
IFACE="$(ip route show default | awk '{print $5; exit}')"
[ -n "$IFACE" ] || exit 0
# Offloads off is the primary fix; the MTU clamp is insurance for low-PMTU paths.
# 1400 is conservative and adjustable — lower it (e.g. 1280) if a host's tunnel
# has a smaller path MTU.
ethtool -K "$IFACE" tso off gso off gro off lro off 2>/dev/null || true
ip link set dev "$IFACE" mtu 1400 2>/dev/null || true
NICTUNE
sudo chmod +x /usr/local/sbin/nic-tune.sh
sudo tee /etc/systemd/system/nic-tune.service >/dev/null <<'NICUNIT'
[Unit]
Description=Tune guest NIC for vmnet NAT egress (disable offloads, clamp MTU)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/nic-tune.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
NICUNIT
# enable --now both installs the boot hook and applies it immediately, so the
# current provision (and the runner about to start) gets working egress.
sudo systemctl daemon-reload || true
sudo systemctl enable --now nic-tune.service || true

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
