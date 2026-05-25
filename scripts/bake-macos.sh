#!/bin/bash
# Bake the unified macOS golden: Cirrus macos-tahoe-xcode + iOS 18 simulator
# runtime + JDK 21 + Android SDK/emulator + arm64-v8a system image. The result
# can pick up either iOS (xcode) or Android (android) jobs, so both of the
# host's two macOS VM slots can handle any mobile workload.
#
# Why iOS 18: Maestro 2.4.0 doesn't track simctl-launched apps correctly on
# iOS 26 simulators (SpringBoard assertions stay inactive; XCUIApplication
# reports "not running" while the app is clearly alive). Pinning smoke tests
# to an iOS 18 device sidesteps that without downgrading Xcode.
#
# Why baked together (and not two goldens): Apple's EULA caps macOS guests at
# two per host. Separate xcode/android goldens pin each slot to one workload
# and stall whenever the queue is lopsided; one merged golden lets either slot
# claim either job.
#
# Usage:
#   scripts/bake-macos.sh                  # -> local VM "macos"
#   scripts/bake-macos.sh my-mobile        # -> local VM "my-mobile"
#
# Env overrides:
#   BAKE_SOURCE          source OCI/local name (default ghcr.io/cirruslabs/macos-tahoe-xcode:latest)
#   IOS_RUNTIME_VERSION  iOS simulator runtime to add (default 18.5)
#   BAKE_CPU             cores during bake (default 6)
#   BAKE_MEM_MB          memory during bake, MB (default 12288)
#   BAKE_DISK_GB         disk size for the golden (default 180 — iOS runtime +8GB, Android SDK +4GB)
set -euo pipefail

readonly HERE="$(cd "$(dirname "$0")" && pwd)"
readonly SOURCE_IMAGE="${BAKE_SOURCE:-ghcr.io/cirruslabs/macos-tahoe-xcode:latest}"
readonly DEST_NAME="${1:-macos}"
readonly IOS_RUNTIME_VERSION="${IOS_RUNTIME_VERSION:-18.5}"
readonly BAKE_CPU="${BAKE_CPU:-6}"
readonly BAKE_MEM_MB="${BAKE_MEM_MB:-12288}"
readonly BAKE_DISK_GB="${BAKE_DISK_GB:-180}"
readonly GUEST_USER=admin
readonly GUEST_PASS=admin
readonly INSTALL_SCRIPT="$HERE/bake/macos-install.sh"

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

IP=$(tart ip "$DEST_NAME" --wait 180)
echo ">>> guest IP: $IP"

echo ">>> waiting for SSH (macOS guests take ~30s to reach login)"
ssh_ready=0
for _ in {1..90}; do
  if ssh_run "$IP" true 2>/dev/null; then ssh_ready=1; break; fi
  sleep 2
done
[[ "$ssh_ready" -eq 1 ]] || { echo "SSH to $IP never came up after 3 min" >&2; exit 1; }

echo ">>> copying installer"
scp_to "$IP" "$INSTALL_SCRIPT" "/tmp/macos-install.sh"

echo ">>> running installer (iOS $IOS_RUNTIME_VERSION runtime ~8GB + Android SDK ~3GB, 25-40 min)"
ssh_run "$IP" "IOS_RUNTIME_VERSION='$IOS_RUNTIME_VERSION' chmod +x /tmp/macos-install.sh && /tmp/macos-install.sh"

echo ">>> shutting guest down"
ssh_run "$IP" "sudo shutdown -h now" || true
wait "$TART_PID" 2>/dev/null || true
trap - EXIT INT TERM

echo ">>> done. Golden VM: $DEST_NAME"
echo "    Next: set runners.yaml -> image: $DEST_NAME, then \`bakery up macos\`"
