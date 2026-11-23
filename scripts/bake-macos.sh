#!/bin/bash
# Bake a macOS golden with optional tool selections from runners.yaml.
# Usage: bakery build macos [--as NAME]
# BAKE_SOURCE, BAKE_CPU, BAKE_MEM_MB, and BAKE_DISK_GB set the base image and resources.
set -euo pipefail

readonly HERE="$(cd "$(dirname "$0")" && pwd)"
readonly SOURCE_IMAGE="${BAKE_SOURCE:-ghcr.io/cirruslabs/macos-golden-gate-xcode:latest}"
readonly DEST_NAME="${1:-macos}"
readonly BAKE_CPU="${BAKE_CPU:-6}"
readonly BAKE_MEM_MB="${BAKE_MEM_MB:-12288}"
readonly BAKE_DISK_GB="${BAKE_DISK_GB:-180}"
export BAKERY_XCODE_DONOR="bakery-xcode-donor-$$"
readonly GUEST_USER=admin
readonly GUEST_PASS=admin
readonly INSTALL_SCRIPT="$HERE/bake/macos-install.sh"
: "${BAKERY_BIN:?Run bakery build macos}"
: "${BAKERY_TOOLS_JSON:?Run bakery build macos}"
command -v tart >/dev/null \
  || { echo "tart not found. Install openai/tools/tart with Homebrew." >&2; exit 1; }
[[ -f "$INSTALL_SCRIPT" ]] \
  || { echo "missing $INSTALL_SCRIPT" >&2; exit 1; }

readonly SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=15 -o PubkeyAuthentication=no -o PreferredAuthentications=password -o IdentitiesOnly=yes)
# On a macOS 27 host, sshpass fails about 1 login in 4: ssh logs "can't open /dev/tty",
# sends empty passwords, and sshd answers "Permission denied".
export SSH_ASKPASS="$HERE/bake/guest-askpass.sh" SSH_ASKPASS_REQUIRE=force BAKE_GUEST_PASS="$GUEST_PASS"
ssh_run()  { ssh "${SSH_OPTS[@]}" "$GUEST_USER@$1" "$2"; }
scp_to()   { scp "${SSH_OPTS[@]}" "$2" "$GUEST_USER@$1:$3"; }

# The EXIT trap stops the golden and deletes an incomplete bake.
VM_NAME="" VM_PID="" VM_IP="" PARTIAL_GOLDEN=""
# Wait up to 2 min for `tart run` to exit, then end it. Sometimes `tart run`
# stays alive after the guest stops, and an unbounded wait hangs the bake.
wait_vm_exit() {
  for _ in $(seq 60); do
    kill -0 "$VM_PID" 2>/dev/null || break
    sleep 2
  done
  kill "$VM_PID" 2>/dev/null || true
  wait "$VM_PID" 2>/dev/null || true
  VM_PID=""
}

cleanup() {
  if [[ -n "$VM_PID" ]] && kill -0 "$VM_PID" 2>/dev/null; then
    tart stop "$VM_NAME" >/dev/null 2>&1 || true
    wait_vm_exit
  fi
  tart stop "$BAKERY_XCODE_DONOR" >/dev/null 2>&1 || true
  tart delete "$BAKERY_XCODE_DONOR" >/dev/null 2>&1 || true
  rm -f "$MANIFEST"
  # `bakery up` clones the golden by name and cannot detect an unfinished bake.
  [[ -z "$PARTIAL_GOLDEN" ]] || tart delete "$PARTIAL_GOLDEN" >/dev/null 2>&1 || true
}
MANIFEST=$(mktemp /tmp/bakery-tools.XXXXXX)
trap cleanup EXIT
trap 'exit 1' INT TERM
printf '%s' "$BAKERY_TOOLS_JSON" > "$MANIFEST"

# Boot <name> and wait until SSH is up. Sets VM_NAME, VM_PID and VM_IP.
boot_vm() {
  local log="/tmp/bake-$1.log"
  VM_NAME="$1"
  echo ">>> booting $VM_NAME (log: $log)"
  tart run --no-graphics "$VM_NAME" >"$log" 2>&1 &
  VM_PID=$!
  VM_IP=$(tart ip "$VM_NAME" --wait 180)
  echo ">>> guest IP: $VM_IP"
  echo ">>> waiting for SSH (macOS guests take ~30s to reach login)"
  for _ in {1..90}; do
    if ssh_run "$VM_IP" true 2>/dev/null; then return 0; fi
    sleep 2
  done
  echo "SSH to $VM_IP never came up after 3 min" >&2
  exit 1
}

shutdown_vm() {
  ssh_run "$VM_IP" "sudo shutdown -h now" || true
  wait_vm_exit
}

echo ">>> cloning $SOURCE_IMAGE -> $DEST_NAME"
tart clone "$SOURCE_IMAGE" "$DEST_NAME"
PARTIAL_GOLDEN="$DEST_NAME"
tart set "$DEST_NAME" --cpu "$BAKE_CPU" --memory "$BAKE_MEM_MB" --disk-size "$BAKE_DISK_GB"
boot_vm "$DEST_NAME"
IP="$VM_IP"

echo ">>> copying installer"
scp_to "$IP" "$INSTALL_SCRIPT" "/tmp/macos-install.sh"

"$BAKERY_BIN" prepare-tools "$IP" "$MANIFEST" "$SOURCE_IMAGE"
scp_to "$IP" "$BAKERY_BIN" /tmp/bakery
scp_to "$IP" "$MANIFEST" /tmp/bakery-tools.json

echo ">>> running installer"
ssh_run "$IP" "chmod +x /tmp/macos-install.sh && /tmp/macos-install.sh"

echo ">>> initializing simulator services and waiting for host recovery"
scp_to "$IP" "$HERE/macos-readiness.py" "/tmp/bakery-readiness.py"
# shellcheck disable=SC2016
ssh_run "$IP" 'mkdir -p "$HOME/bakery-diagnostics"; set -o pipefail; python3 /tmp/bakery-readiness.py 2>&1 | tee "$HOME/bakery-diagnostics/bake.log"'

echo ">>> shutting guest down"
shutdown_vm
PARTIAL_GOLDEN=""

echo ">>> done. Golden VM: $DEST_NAME"
echo "    Next: set runners.yaml -> image: $DEST_NAME, then \`bakery up macos\`"
