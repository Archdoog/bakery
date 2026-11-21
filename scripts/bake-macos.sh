#!/bin/bash
# Bake a macOS golden with the simulator selection from runners.yaml.
# Usage: bakery build macos [--as NAME]
# BAKE_SOURCE, BAKE_CPU, BAKE_MEM_MB, and BAKE_DISK_GB set the base image and resources.
set -euo pipefail

readonly HERE="$(cd "$(dirname "$0")" && pwd)"
readonly SOURCE_IMAGE="${BAKE_SOURCE:-ghcr.io/cirruslabs/macos-tahoe-xcode:27.2}"
readonly DEST_NAME="${1:-macos}"
readonly BAKE_CPU="${BAKE_CPU:-6}"
readonly BAKE_MEM_MB="${BAKE_MEM_MB:-12288}"
readonly BAKE_DISK_GB="${BAKE_DISK_GB:-180}"
readonly RUNTIME_DONOR_NAME="bakery-runtime-donor-$$"
readonly GUEST_USER=admin
readonly GUEST_PASS=admin
readonly INSTALL_SCRIPT="$HERE/bake/macos-install.sh"
: "${BAKERY_BIN:?Run bakery build macos with a runners.yaml configuration}"
: "${BAKERY_SIMULATORS_JSON:?Run bakery build macos with a runners.yaml configuration}"
command -v tart >/dev/null \
  || { echo "tart not found — brew install cirruslabs/cli/tart" >&2; exit 1; }
[[ -f "$INSTALL_SCRIPT" ]] \
  || { echo "missing $INSTALL_SCRIPT" >&2; exit 1; }

readonly SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=15 -o PubkeyAuthentication=no -o PreferredAuthentications=password -o IdentitiesOnly=yes)
# On a macOS 27 host, sshpass fails about 1 login in 4: ssh logs "can't open /dev/tty",
# sends empty passwords, and sshd answers "Permission denied".
export SSH_ASKPASS="$HERE/bake/guest-askpass.sh" SSH_ASKPASS_REQUIRE=force BAKE_GUEST_PASS="$GUEST_PASS"
ssh_run()  { ssh "${SSH_OPTS[@]}" "$GUEST_USER@$1" "$2"; }
scp_to()   { scp "${SSH_OPTS[@]}" "$2" "$GUEST_USER@$1:$3"; }
scp_from() { scp "${SSH_OPTS[@]}" "$GUEST_USER@$1:$2" "$3"; }

# One guest runs at a time. The EXIT trap stops it and deletes the donor.
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
  rm -f "$MANIFEST" "$MANIFEST.fields" "$MANIFEST.apple" "$MANIFEST.xcodes"
  if [[ -n "${DONOR_CREATED:-}" ]]; then
    tart delete "$RUNTIME_DONOR_NAME" >/dev/null 2>&1 || true
  fi
  # `bakery up` clones the golden by name and cannot detect an unfinished bake.
  [[ -z "$PARTIAL_GOLDEN" ]] || tart delete "$PARTIAL_GOLDEN" >/dev/null 2>&1 || true
}
MANIFEST=$(mktemp /tmp/bakery-simulators.XXXXXX)
trap cleanup EXIT
trap 'exit 1' INT TERM
printf '%s' "$BAKERY_SIMULATORS_JSON" > "$MANIFEST"
APPLE_JSON="${BAKERY_APPLE_JSON:-}"
[[ -n "$APPLE_JSON" ]] || APPLE_JSON='{}'
printf '%s' "$APPLE_JSON" > "$MANIFEST.apple"

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

wait_simulator_services() {
  ssh_run "$VM_IP" 'for _ in $(seq 60); do xcrun simctl list runtimes >/dev/null 2>&1 && xcrun simctl runtime list >/dev/null 2>&1 && exit 0; sleep 5; done; echo "simctl did not answer after 5 min" >&2; exit 1'
}

download_runtime_image() {
  local dest="$1" platform="$2" version="$3" source="$4" cached_before remote
  cached_before=$(tart list --source oci --quiet)
  echo ">>> no $platform $version runtime image on this host"
  echo ">>> downloading it with $source (one time; the image pull is ~70GB)"
  DONOR_CREATED=1
  tart clone "$source" "$RUNTIME_DONOR_NAME"
  boot_vm "$RUNTIME_DONOR_NAME"
  echo ">>> waiting for the guest's simulator services"
  wait_simulator_services
  for attempt in 1 2 3; do
    ssh_run "$VM_IP" "rm -rf /tmp/runtime && mkdir /tmp/runtime && xcodebuild -downloadPlatform '$platform' -buildVersion '$version' -architectureVariant arm64 -exportPath /tmp/runtime" && break
    [[ $attempt -lt 3 ]] || { echo "$platform $version download failed 3 times" >&2; exit 1; }
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

echo ">>> cloning $SOURCE_IMAGE -> $DEST_NAME"
tart clone "$SOURCE_IMAGE" "$DEST_NAME"
PARTIAL_GOLDEN="$DEST_NAME"
tart set "$DEST_NAME" --cpu "$BAKE_CPU" --memory "$BAKE_MEM_MB" --disk-size "$BAKE_DISK_GB"
boot_vm "$DEST_NAME"
IP="$VM_IP"

echo ">>> copying installer"
scp_to "$IP" "$INSTALL_SCRIPT" "/tmp/macos-install.sh"

scp_to "$IP" "$BAKERY_BIN" /tmp/bakery
ssh_run "$IP" 'chmod +x /tmp/bakery'
"$BAKERY_BIN" xcodes fields "$MANIFEST.apple" > "$MANIFEST.xcodes"
while IFS= read -r -d '' xcode_version && IFS= read -r -d '' xcode_source; do
  if ssh_run "$IP" "/tmp/bakery xcodes contains '$xcode_version'"; then
    continue
  fi
  archive="$(dirname "$HERE")/state/xcode-${xcode_version}.tar"
  if [[ ! -f "$archive" ]]; then
    # The donor and golden must run separately to preserve the guest limit.
    shutdown_vm
    cached_before=$(tart list --source oci --quiet)
    DONOR_CREATED=1
    tart clone "$xcode_source" "$RUNTIME_DONOR_NAME"
    boot_vm "$RUNTIME_DONOR_NAME"
    scp_to "$VM_IP" "$BAKERY_BIN" /tmp/bakery
    ssh_run "$VM_IP" "chmod +x /tmp/bakery && /tmp/bakery xcodes export '$xcode_version' /tmp/bakery-xcode.tar"
    mkdir -p "$(dirname "$archive")"
    scp_from "$VM_IP" /tmp/bakery-xcode.tar "$archive.part"
    mv "$archive.part" "$archive"
    shutdown_vm
    tart delete "$RUNTIME_DONOR_NAME"
    comm -13 <(sort <<<"$cached_before") <(tart list --source oci --quiet | sort) \
      | while read -r entry; do tart delete "$entry" || true; done
    boot_vm "$DEST_NAME"
    IP="$VM_IP"
  fi
  scp_to "$IP" "$archive" "/tmp/bakery-xcode-${xcode_version}.tar"
done < "$MANIFEST.xcodes"
scp_to "$IP" "$MANIFEST.apple" /tmp/bakery-apple.json
ssh_run "$IP" '/tmp/bakery xcodes install /tmp/bakery-apple.json'

echo ">>> preparing simulator runtime images"
wait_simulator_services
"$BAKERY_BIN" simulators fields "$MANIFEST" > "$MANIFEST.fields"
while IFS= read -r -d '' index \
  && IFS= read -r -d '' platform \
  && IFS= read -r -d '' version \
  && IFS= read -r -d '' runtime_image \
  && IFS= read -r -d '' donor_source \
  && IFS= read -r -d '' runtime_xcode; do
  runtime_env=""
  if [[ -n "$runtime_xcode" ]]; then
    runtime_env="env DEVELOPER_DIR='/Applications/Xcode-${runtime_xcode}.app/Contents/Developer' "
  fi
  if ssh_run "$IP" "${runtime_env}xcrun simctl list runtimes -j" \
    | "$BAKERY_BIN" simulators contains "$platform" "$version"; then
    continue
  fi
  cache="$(dirname "$HERE")/state/$(printf '%s' "$platform" | tr '[:upper:]' '[:lower:]')-${version}-simulator-runtime.dmg"
  if [[ -z "$runtime_image" && -f "$cache" ]]; then
    runtime_image="$cache"
  fi
  if [[ -z "$runtime_image" ]] && xcrun -f simctl >/dev/null 2>&1; then
    runtime_image=$(xcrun simctl runtime list -j \
      | "$BAKERY_BIN" simulators find-image "$platform" "$version")
  fi
  if [[ -z "$runtime_image" && -n "$donor_source" ]]; then
    # The donor and golden must run separately to preserve the guest limit.
    shutdown_vm
    download_runtime_image "$cache" "$platform" "$version" "$donor_source"
    runtime_image="$cache"
    boot_vm "$DEST_NAME"
    IP="$VM_IP"
    wait_simulator_services
  fi
  if [[ -n "$runtime_image" ]]; then
    [[ -f "$runtime_image" ]] || { echo "Runtime image not found: $runtime_image" >&2; exit 1; }
    scp_to "$IP" "$runtime_image" "/tmp/bakery-runtime-${index}.dmg"
  fi
done < "$MANIFEST.fields"

scp_to "$IP" "$MANIFEST" /tmp/bakery-simulators.json
scp_to "$IP" "$BAKERY_BIN" /tmp/bakery

echo ">>> running installer"
ssh_run "$IP" "chmod +x /tmp/macos-install.sh && /tmp/macos-install.sh"

echo ">>> shutting guest down"
shutdown_vm
PARTIAL_GOLDEN=""

echo ">>> done. Golden VM: $DEST_NAME"
echo "    Next: set runners.yaml -> image: $DEST_NAME, then \`bakery up macos\`"
