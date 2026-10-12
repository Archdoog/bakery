#!/bin/bash
# Bake the unified macOS golden: Cirrus macos-tahoe-xcode (Xcode 27) + iOS 27
# and iOS 18.6 simulator runtimes + JDK 21 + Android SDK/emulator + arm64-v8a
# system image. The result can pick up either iOS (xcode) or Android (android)
# jobs, so both of the host's two macOS VM slots can handle any mobile workload.
#
# Why iOS 18.6: Maestro smoke tests run on iOS 18. Xcode 27 does not download
# iOS 18 runtimes, so the bake copies a signed runtime image from the host into
# the guest. If the host has no image, the bake gets one with a temporary
# Xcode 26 guest and caches it in state/. See find_runtime_image below.
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
#   BAKE_SOURCE          source OCI/local name (default ghcr.io/cirruslabs/macos-tahoe-xcode:27)
#   IOS_RUNTIME_VERSION  iOS simulator runtime to pin (default 27.0)
#   MAESTRO_IOS_RUNTIME_VERSION  iOS runtime for Maestro smoke tests (default 18.6)
#   MAESTRO_IOS_RUNTIME_DMG      host path of that runtime's disk image (default:
#                        state/ios-<version>-simulator-runtime.dmg, then the
#                        host Xcode's runtime, then a download)
#   RUNTIME_DONOR_SOURCE  Xcode 26 image that downloads the runtime if the host
#                        has none (default ghcr.io/cirruslabs/macos-tahoe-xcode:26.5)
#   BAKE_CPU             cores during bake (default 6)
#   BAKE_MEM_MB          memory during bake, MB (default 12288)
#   BAKE_DISK_GB         disk size for the golden (default 180 — two iOS runtimes +16GB, Android SDK +4GB)
set -euo pipefail

readonly HERE="$(cd "$(dirname "$0")" && pwd)"
readonly SOURCE_IMAGE="${BAKE_SOURCE:-ghcr.io/cirruslabs/macos-tahoe-xcode:27}"
readonly DEST_NAME="${1:-macos}"
readonly IOS_RUNTIME_VERSION="${IOS_RUNTIME_VERSION:-27.0}"
readonly MAESTRO_IOS_RUNTIME_VERSION="${MAESTRO_IOS_RUNTIME_VERSION:-18.6}"
readonly BAKE_CPU="${BAKE_CPU:-6}"
readonly BAKE_MEM_MB="${BAKE_MEM_MB:-12288}"
readonly BAKE_DISK_GB="${BAKE_DISK_GB:-180}"
readonly RUNTIME_DONOR_SOURCE="${RUNTIME_DONOR_SOURCE:-ghcr.io/cirruslabs/macos-tahoe-xcode:26.5}"
readonly RUNTIME_DONOR_NAME="bakery-runtime-donor"
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
ssh_run()  { sshpass -p "$GUEST_PASS" ssh "${SSH_OPTS[@]}" "$GUEST_USER@$1" "$2"; }
scp_to()   { sshpass -p "$GUEST_PASS" scp "${SSH_OPTS[@]}" "$2" "$GUEST_USER@$1:$3"; }
scp_from() { sshpass -p "$GUEST_PASS" scp "${SSH_OPTS[@]}" "$GUEST_USER@$1:$2" "$3"; }

# One guest runs at a time. The EXIT trap stops it and deletes the donor.
VM_NAME="" VM_PID="" VM_IP=""
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
  tart delete "$RUNTIME_DONOR_NAME" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

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

# Download the iOS runtime image with a temporary Xcode 26 guest and save it
# to $1. Xcode 26 can download iOS 18 runtimes, but Xcode 27 cannot. After the
# download, delete the guest and the image cache entries that its pull added.
download_runtime_image() {
  local dest="$1" cached_before remote
  cached_before=$(tart list --source oci --quiet)
  echo ">>> no iOS $MAESTRO_IOS_RUNTIME_VERSION runtime image on this host"
  echo ">>> downloading it with $RUNTIME_DONOR_SOURCE (one time; the image pull is ~70GB)"
  tart delete "$RUNTIME_DONOR_NAME" >/dev/null 2>&1 || true
  tart clone "$RUNTIME_DONOR_SOURCE" "$RUNTIME_DONOR_NAME"
  boot_vm "$RUNTIME_DONOR_NAME"
  # SSH comes up before the guest's auto-login finishes. Until then, xcodebuild
  # stops with "Unable to connect to simulator". Wait for simctl to answer, and
  # retry the download if the services are still not stable.
  echo ">>> waiting for the guest's simulator services"
  ssh_run "$VM_IP" 'for _ in $(seq 60); do xcrun simctl list runtimes >/dev/null 2>&1 && xcrun simctl runtime list >/dev/null 2>&1 && exit 0; sleep 5; done; echo "simctl did not answer after 5 min" >&2; exit 1'
  for attempt in 1 2 3; do
    ssh_run "$VM_IP" "rm -rf /tmp/runtime && mkdir /tmp/runtime && xcodebuild -downloadPlatform iOS -buildVersion '$MAESTRO_IOS_RUNTIME_VERSION' -exportPath /tmp/runtime" && break
    [[ $attempt -lt 3 ]] || { echo "iOS $MAESTRO_IOS_RUNTIME_VERSION download failed 3 times" >&2; exit 1; }
    echo ">>> download attempt $attempt failed, trying again in 30s"
    sleep 30
  done
  remote=$(ssh_run "$VM_IP" "find /tmp/runtime -name '*.dmg' | head -1")
  [[ -n "$remote" ]] || { echo "xcodebuild exported no .dmg to /tmp/runtime" >&2; exit 1; }
  echo ">>> copying $remote to $dest"
  mkdir -p "$(dirname "$dest")"
  scp_from "$VM_IP" "$remote" "$dest.part"
  mv "$dest.part" "$dest"
  shutdown_vm
  tart delete "$RUNTIME_DONOR_NAME"
  comm -13 <(sort <<<"$cached_before") <(tart list --source oci --quiet | sort) \
    | while read -r entry; do tart delete "$entry" || true; done
}

# Set RUNTIME_IMAGE to the iOS $MAESTRO_IOS_RUNTIME_VERSION runtime image. Use the
# first that is set: MAESTRO_IOS_RUNTIME_DMG, the cached copy in state/, the
# runtime from this host's Xcode, or a new download into state/. A host with
# only Command Line Tools has no simctl, so it uses the cache or the download.
find_runtime_image() {
  local cache found=""
  cache="$(dirname "$HERE")/state/ios-$MAESTRO_IOS_RUNTIME_VERSION-simulator-runtime.dmg"
  if [[ -n "${MAESTRO_IOS_RUNTIME_DMG:-}" ]]; then
    found="$MAESTRO_IOS_RUNTIME_DMG"
  elif [[ -f "$cache" ]]; then
    found="$cache"
  elif xcrun -f simctl >/dev/null 2>&1; then
    found=$(xcrun simctl runtime list -j | /usr/bin/python3 -c "
import json, sys
version = sys.argv[1]
for img in json.load(sys.stdin).values():
    if img.get('version') == version and img.get('state') == 'Ready':
        print(img.get('path', '')); break
" "$MAESTRO_IOS_RUNTIME_VERSION")
  fi
  if [[ -z "$found" ]]; then
    download_runtime_image "$cache"
    found="$cache"
  fi
  [[ -f "$found" ]] || { echo "iOS runtime image not found: $found" >&2; exit 1; }
  RUNTIME_IMAGE="$found"
}

find_runtime_image
echo ">>> iOS $MAESTRO_IOS_RUNTIME_VERSION runtime image: $RUNTIME_IMAGE"

echo ">>> cloning $SOURCE_IMAGE -> $DEST_NAME"
tart clone "$SOURCE_IMAGE" "$DEST_NAME"
tart set "$DEST_NAME" --cpu "$BAKE_CPU" --memory "$BAKE_MEM_MB" --disk-size "$BAKE_DISK_GB"
boot_vm "$DEST_NAME"
IP="$VM_IP"

echo ">>> copying installer"
scp_to "$IP" "$INSTALL_SCRIPT" "/tmp/macos-install.sh"

echo ">>> copying iOS $MAESTRO_IOS_RUNTIME_VERSION runtime image (~9GB)"
scp_to "$IP" "$RUNTIME_IMAGE" "/tmp/maestro-ios-runtime.dmg"

echo ">>> running installer (iOS $IOS_RUNTIME_VERSION + $MAESTRO_IOS_RUNTIME_VERSION runtimes ~16GB + Android SDK ~3GB, 30-50 min)"
ssh_run "$IP" "chmod +x /tmp/macos-install.sh && IOS_RUNTIME_VERSION='$IOS_RUNTIME_VERSION' MAESTRO_IOS_RUNTIME_VERSION='$MAESTRO_IOS_RUNTIME_VERSION' MAESTRO_IOS_RUNTIME_DMG=/tmp/maestro-ios-runtime.dmg /tmp/macos-install.sh"

echo ">>> shutting guest down"
shutdown_vm

echo ">>> done. Golden VM: $DEST_NAME"
echo "    Next: set runners.yaml -> image: $DEST_NAME, then \`bakery up macos\`"
