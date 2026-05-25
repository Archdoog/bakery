#!/bin/bash
# Bake a Linux golden VM: Temurin 21, Node 24 LTS + corepack, uv, rustup.
# Clones ghcr.io/cirruslabs/ubuntu:latest, provisions over SSH, and leaves a
# stopped VM named by the first argument (default: "ubuntu").
#
# Usage:
#   scripts/bake-ubuntu.sh               # -> local VM "ubuntu"
#   scripts/bake-ubuntu.sh my-ubuntu     # -> local VM "my-ubuntu"
#
# Env overrides:
#   BAKE_CPU       cores during bake (default 6)
#   BAKE_MEM_MB    memory during bake, MB (default 8192)
#   BAKE_DISK_GB   disk size for the golden (default 40)
set -euo pipefail

readonly HERE="$(cd "$(dirname "$0")" && pwd)"
readonly SOURCE_IMAGE="ghcr.io/cirruslabs/ubuntu:latest"
readonly DEST_NAME="${1:-ubuntu}"
readonly BAKE_CPU="${BAKE_CPU:-6}"
readonly BAKE_MEM_MB="${BAKE_MEM_MB:-8192}"
readonly BAKE_DISK_GB="${BAKE_DISK_GB:-40}"
readonly GUEST_USER=admin
readonly GUEST_PASS=admin
readonly INSTALL_SCRIPT="$HERE/bake/ubuntu-install.sh"

command -v sshpass >/dev/null \
  || { echo "sshpass not found — brew install sshpass" >&2; exit 1; }
command -v tart >/dev/null \
  || { echo "tart not found — brew install cirruslabs/cli/tart" >&2; exit 1; }
[[ -f "$INSTALL_SCRIPT" ]] \
  || { echo "missing $INSTALL_SCRIPT" >&2; exit 1; }

readonly SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=15 -o PubkeyAuthentication=no -o PreferredAuthentications=password -o IdentitiesOnly=yes)
ssh_run() { sshpass -p "$GUEST_PASS" ssh "${SSH_OPTS[@]}" "$GUEST_USER@$1" "$2"; }
scp_to()  { sshpass -p "$GUEST_PASS" scp "${SSH_OPTS[@]}" "$2" "$GUEST_USER@$1:$3"; }

echo ">>> cloning $SOURCE_IMAGE -> $DEST_NAME"
tart clone "$SOURCE_IMAGE" "$DEST_NAME"
tart set "$DEST_NAME" --cpu "$BAKE_CPU" --memory "$BAKE_MEM_MB" --disk-size "$BAKE_DISK_GB"

LOG="/tmp/bake-$DEST_NAME.log"
echo ">>> booting $DEST_NAME (log: $LOG)"
tart run --no-graphics "$DEST_NAME" >"$LOG" 2>&1 &
TART_PID=$!
cleanup() {
  if kill -0 "$TART_PID" 2>/dev/null; then
    tart stop "$DEST_NAME" >/dev/null 2>&1 || true
    wait "$TART_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

IP=$(tart ip "$DEST_NAME" --wait 120)
echo ">>> guest IP: $IP"

echo ">>> waiting for SSH (cloud-init brings up networking first)"
ssh_ready=0
for _ in {1..90}; do
  if ssh_run "$IP" true 2>/dev/null; then ssh_ready=1; break; fi
  sleep 2
done
[[ "$ssh_ready" -eq 1 ]] || { echo "SSH to $IP never came up after 3 min" >&2; exit 1; }

echo ">>> copying installer"
scp_to "$IP" "$INSTALL_SCRIPT" "/tmp/ubuntu-install.sh"

echo ">>> running installer (several minutes)"
ssh_run "$IP" "chmod +x /tmp/ubuntu-install.sh && /tmp/ubuntu-install.sh"

echo ">>> shutting guest down"
ssh_run "$IP" "sudo shutdown -h now" || true
wait "$TART_PID" 2>/dev/null || true
trap - EXIT INT TERM

echo ">>> done. Golden VM: $DEST_NAME"
echo "    Next: set runners.yaml -> image: $DEST_NAME"
