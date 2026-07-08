#!/bin/bash
# Runs inside a macOS aarch64 guest (cloned from macos-tahoe-xcode) during
# `bake-macos.sh`. Layers on top of the stock Cirrus Xcode image:
#   - iOS simulator runtime $IOS_RUNTIME_VERSION (default 18.5) + iPhone 16 device
#     (Maestro), plus a warm iPhone 17 Pro Max on the pinned iOS
#     $IOS_SNAPSHOT_RUNTIME_VERSION (default 26.5) runtime (Rallista-iOS-V3
#     snapshot + XCUITest CI target)
#   - Temurin JDK 21 (Homebrew cask)
#   - Android cmdline-tools + platform-tools + platforms;android-34 + build-tools;34.0.0
#   - Android emulator + arm64-v8a system image (API 34)
#   - Android NDK $ANDROID_NDK_VERSION (default 26.2.11394342) for Rust↔JNI builds
#   - rustup + stable toolchain (default profile: rustfmt + clippy included)
#     with iOS (3) and Android (4) targets
#   - cargo-ndk CLI for invoking cargo across the four Android ABIs
#   - just (command runner) via Homebrew
#   - Node.js 24 LTS (Homebrew) + corepack (pnpm/yarn resolved per-repo)
#   - uv (Astral Python installer)
# Writes toolchain paths into ~/.zshrc.
#
# Node and Python are present here for iOS/Android jobs that shell out to
# node/python helpers (Maestro test scripts, code generators, Fastlane
# plugins, etc.). The TS/Python runner classes still live on ubuntu — see
# runners.yaml.example. We don't add `node` / `python` labels here because
# macOS guests are EULA-capped at 2 per host and shouldn't be consumed by
# work that has no macOS dependency.
#
# The stock image ships Xcode with iOS 26.x only; iOS 18 is added alongside so
# Maestro smoke tests have a compatible simulator to target.
#
# Note: on M1/M2 hosts the Android emulator will run in TCG (software) mode
# because there's no nested virtualization — expect 10-50x slower instrumentation
# tests than on bare metal. Full acceleration requires M3+ with tart's
# --nested-virtualization enabled.
set -euo pipefail

# Non-interactive SSH doesn't source ~/.zprofile; put Apple Silicon brew on PATH.
eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null)" || true

IOS_RUNTIME_VERSION="${IOS_RUNTIME_VERSION:-18.5}"
# Exact iOS runtime Rallista-iOS-V3 records snapshots on and runs XCUITests on.
# Pinned to a full point release (not just "26") so the golden is deterministic:
# CI targets `iPhone 17 Pro Max (26.5)`, so this must be the 26.5 runtime, not
# whatever 26.x Xcode happens to bundle. Bumping it here and in the repo's
# Fastfile destination is a two-line, coordinated change.
IOS_SNAPSHOT_RUNTIME_VERSION="${IOS_SNAPSHOT_RUNTIME_VERSION:-26.5}"
ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ANDROID_NDK_VERSION="${ANDROID_NDK_VERSION:-26.2.11394342}"
CMDLINE_TOOLS_BUILD="${CMDLINE_TOOLS_BUILD:-11076708}"

command -v xcodebuild >/dev/null \
  || { echo "xcodebuild missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }
command -v brew >/dev/null \
  || { echo "brew missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }

# ---- sshd: disable PerSourcePenalties (cold-clone provisioning races) -------
# OpenSSH 9.8+ (shipped in this macOS) penalizes a source IP after failed auth
# attempts. A freshly stamped clone accepts SSH connections before
# `opendirectory` is ready to authenticate, so bakery's provisioning probes
# fail during that window and jail the host's IP — making the real `scp` fail
# with "Permission denied" even after the guest is up. These are ephemeral CI
# guests on a private vmnet, so the penalty buys no security. Bake it off so
# every clone is provisionable the moment auth comes up. Written both as a
# drop-in and (idempotently) in the main config so it takes effect whether or
# not sshd_config Includes the drop-in dir; both set "no", so precedence is moot.
echo ">>> disabling sshd PerSourcePenalties (cold-clone provisioning races)"
sudo mkdir -p /etc/ssh/sshd_config.d
printf 'PerSourcePenalties no\n' \
  | sudo tee /etc/ssh/sshd_config.d/99-bakery-no-penalty.conf >/dev/null
if ! sudo grep -qE '^[[:space:]]*PerSourcePenalties[[:space:]]+no' /etc/ssh/sshd_config 2>/dev/null; then
  printf '\nPerSourcePenalties no\n' | sudo tee -a /etc/ssh/sshd_config >/dev/null
fi
sudo sshd -t && echo ">>> sshd config OK" || echo ">>> WARN: sshd -t flagged the config"

# ---- iOS simulator runtime -------------------------------------------------

echo ">>> Xcode info"
xcodebuild -version

echo ">>> runtimes before"
xcrun simctl list runtimes || true

echo ">>> downloading iOS $IOS_RUNTIME_VERSION simulator runtime (~8GB, 15-25 min)"
# Xcode 16+ supports pinning a specific version via -buildVersion. The runtime
# ships as a signed distribution, no Apple ID needed.
xcodebuild -downloadPlatform iOS -buildVersion "$IOS_RUNTIME_VERSION"

# Pin the exact iOS $IOS_SNAPSHOT_RUNTIME_VERSION runtime for the iOS-V3 CI
# device. No-op if the base Xcode already bundles it; otherwise downloads it.
# NOTE: a runtime newer than the base Xcode's SDK is rejected — if this fails,
# the golden's Xcode base image is too old for $IOS_SNAPSHOT_RUNTIME_VERSION;
# bump BAKE_SOURCE to a newer macos-tahoe-xcode tag.
echo ">>> downloading iOS $IOS_SNAPSHOT_RUNTIME_VERSION simulator runtime (iOS-V3 CI)"
xcodebuild -downloadPlatform iOS -buildVersion "$IOS_SNAPSHOT_RUNTIME_VERSION"

echo ">>> runtimes after"
xcrun simctl list runtimes

# ---- simulator devices (baked warm) ----------------------------------------
# Two devices are baked so both mobile workloads land on a pinned, pre-created
# target instead of an ad-hoc cold sim on first CI use:
#   - iPhone 16 (iOS ${IOS_RUNTIME_VERSION})   — Maestro smoke tests. iOS 18
#     sidesteps Maestro's iOS-26 app-tracking bug (see bake-macos.sh header).
#   - iPhone 17 Pro Max (iOS ${IOS_SNAPSHOT_RUNTIME_VERSION}) — Rallista-iOS-V3
#     snapshot + XCUITest CI. Its snapshot references are recorded on this exact
#     device/OS, so we pin the full point release here and CI targets
#     `iPhone 17 Pro Max (${IOS_SNAPSHOT_RUNTIME_VERSION})` — no drift either side.
# Warm-booting each device once during the bake populates its data container
# (SpringBoard first-run setup, the accessibility server, caches) into the
# golden, so the first boot on a fresh clone comes up fast. That head start is
# what keeps XCUITest's "AX loaded" handshake from timing out on a cold boot
# under a resource-constrained guest — the failure mode this bake is tuned for.

# Resolve an available iOS runtime identifier by version prefix ("18.5", "26.5").
runtime_id_for() {
    xcrun simctl list runtimes -j | /usr/bin/python3 -c "
import json, sys
prefix = sys.argv[1]
runtimes = json.load(sys.stdin)['runtimes']
match = next(
    (r for r in runtimes
     if r.get('isAvailable')
     and r.get('platform') == 'iOS'
     and r.get('version', '').startswith(prefix)),
    None,
)
if match is None:
    sys.exit(f'no available iOS {prefix} runtime found')
print(match['identifier'])
" "$1"
}

# Resolve the UDID of the device named <name> *on runtime <runtime-id>* (empty
# if absent). Scoped to the runtime because a bare name like "iPhone 17 Pro Max"
# can exist on several runtimes at once (stock set + our pinned one); a
# name-only lookup would be ambiguous and `simctl` would refuse to act on it.
udid_on_runtime() {
    xcrun simctl list devices -j | /usr/bin/python3 -c "
import json, sys
rid, name = sys.argv[1], sys.argv[2]
for d in json.load(sys.stdin)['devices'].get(rid, []):
    if d.get('name') == name:
        print(d.get('udid', '')); break
" "$1" "$2" 2>/dev/null || true
}

# Create <name> from <device-type> on <runtime-id> unless that runtime already
# has a device with that exact name. Idempotent across re-bakes and Xcode's
# default set, and never mints an ambiguous twin on the same runtime.
ensure_device() {
    local name="$1" device_type="$2" runtime_id="$3"
    if [[ -n "$(udid_on_runtime "$runtime_id" "$name")" ]]; then
        echo ">>> simulator '$name' already exists on $runtime_id, skipping create"
    else
        echo ">>> creating simulator '$name' on $runtime_id"
        xcrun simctl create "$name" "$device_type" "$runtime_id"
    fi
}

# Boot the <name> device on <runtime-id> by UDID, poll until Booted (cap ~120s),
# then shut it down. The booted state itself doesn't survive the clone, but the
# warmed data container does. Bounded poll rather than an unbounded `simctl
# bootstatus` so a wedged headless boot can't hang the non-interactive bake;
# every step is best-effort.
warm_boot() {
    local name="$1" runtime_id="$2" udid="" state=""
    udid="$(udid_on_runtime "$runtime_id" "$name")"
    if [[ -z "$udid" ]]; then
        echo ">>> WARN: '$name' not found on $runtime_id; skipping warm boot"
        return 0
    fi
    echo ">>> warm-booting '$name' ($udid)"
    xcrun simctl boot "$udid" 2>/dev/null || true
    for _ in {1..60}; do
        state=$(xcrun simctl list devices -j | /usr/bin/python3 -c "
import json, sys
udid = sys.argv[1]
for v in json.load(sys.stdin)['devices'].values():
    for d in v:
        if d.get('udid') == udid:
            print(d.get('state', '')); sys.exit()
" "$udid" 2>/dev/null || true)
        [[ "$state" == "Booted" ]] && break
        sleep 2
    done
    echo ">>> '$name' state: ${state:-unknown}"
    xcrun simctl shutdown "$udid" 2>/dev/null || true
}

# iPhone 16 on the downloaded iOS 18.5 runtime (Maestro smoke tests).
RUNTIME_ID_18=$(runtime_id_for "$IOS_RUNTIME_VERSION")
echo ">>> iOS $IOS_RUNTIME_VERSION runtime id: $RUNTIME_ID_18"
ensure_device "iPhone 16 (iOS ${IOS_RUNTIME_VERSION})" \
    "com.apple.CoreSimulator.SimDeviceType.iPhone-16" "$RUNTIME_ID_18"
warm_boot "iPhone 16 (iOS ${IOS_RUNTIME_VERSION})" "$RUNTIME_ID_18"

# iPhone 17 Pro Max on the pinned iOS $IOS_SNAPSHOT_RUNTIME_VERSION runtime
# (Rallista-iOS-V3 CI target). Bare name to match the repo's device string; the
# runtime-scoped helpers keep it distinct from any same-named stock device on a
# different 26.x runtime, and CI's `(${IOS_SNAPSHOT_RUNTIME_VERSION})` suffix
# selects this exact one.
RUNTIME_ID_SNAP=$(runtime_id_for "$IOS_SNAPSHOT_RUNTIME_VERSION")
echo ">>> iOS $IOS_SNAPSHOT_RUNTIME_VERSION runtime id: $RUNTIME_ID_SNAP"
ensure_device "iPhone 17 Pro Max" \
    "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro-Max" "$RUNTIME_ID_SNAP"
warm_boot "iPhone 17 Pro Max" "$RUNTIME_ID_SNAP"

echo ">>> available devices:"
xcrun simctl list devices available || true

# ---- Rust + just -----------------------------------------------------------

echo ">>> rustup + stable (default profile: rustfmt + clippy)"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --default-toolchain stable --profile default >/dev/null

# rustup writes binaries under ~/.cargo/bin; non-interactive SSH skips ~/.zprofile.
export PATH="$HOME/.cargo/bin:$PATH"

# iOS (3) for XCFramework slices, Android (4) for the ABIs cargo-ndk drives.
echo ">>> rust targets (iOS + Android)"
rustup target add \
  aarch64-apple-ios \
  aarch64-apple-ios-sim \
  x86_64-apple-ios \
  aarch64-linux-android \
  armv7-linux-androideabi \
  i686-linux-android \
  x86_64-linux-android

echo ">>> just (Homebrew)"
brew install --quiet just

# ---- Node.js + corepack ----------------------------------------------------

echo ">>> Node.js 24 LTS (Homebrew) + corepack"
brew install --quiet node@24
# node@24 is keg-only; link forcibly so plain `node` / `npm` / `corepack`
# resolve on PATH for jobs that don't source a setup-node action.
brew link --overwrite --force node@24 >/dev/null
# corepack ships with Node 24 but is disabled by default. Enabling it lets
# each project pull the pnpm/yarn version pinned in its package.json's
# `packageManager` field — no global install, no version drift across repos.
corepack enable

# ---- Python (uv) -----------------------------------------------------------

echo ">>> uv (Astral Python)"
curl -LsSf https://astral.sh/uv/install.sh | sh >/dev/null

# ---- Android toolchain -----------------------------------------------------

echo ">>> Temurin JDK 21 (Homebrew cask)"
brew install --quiet --cask temurin@21
JAVA_HOME="$(/usr/libexec/java_home -v 21)"
export JAVA_HOME

echo ">>> Android cmdline-tools"
mkdir -p "$ANDROID_HOME/cmdline-tools"
curl -fsSL -o /tmp/cmdline-tools.zip \
  "https://dl.google.com/android/repository/commandlinetools-mac-${CMDLINE_TOOLS_BUILD}_latest.zip"
unzip -q /tmp/cmdline-tools.zip -d "$ANDROID_HOME/cmdline-tools"
mv "$ANDROID_HOME/cmdline-tools/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
rm /tmp/cmdline-tools.zip

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"

echo ">>> accepting SDK licenses + installing packages (emulator + system image are large)"
# `yes | ...` trips pipefail via SIGPIPE; bound the y's instead.
printf 'y\n%.0s' {1..50} | "$SDKMANAGER" --licenses >/dev/null
"$SDKMANAGER" --install \
  "platform-tools" \
  "platforms;android-34" \
  "build-tools;34.0.0" \
  "emulator" \
  "system-images;android-34;google_apis;arm64-v8a" \
  "ndk;${ANDROID_NDK_VERSION}" >/dev/null

ANDROID_NDK_HOME="$ANDROID_HOME/ndk/${ANDROID_NDK_VERSION}"

echo ">>> cargo-ndk (CLI used by build scripts and the gradle plugin)"
cargo install cargo-ndk --locked >/dev/null

echo ">>> shell profile (~/.zshrc)"
cat <<EOF >> "$HOME/.zshrc"

# --- bakery golden ---
# corepack's download prompt defaults to y/n interactive; non-interactive CI
# shells never answer and hang until GHA times out. Force auto-proceed.
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
export ANDROID_HOME=$ANDROID_HOME
export ANDROID_SDK_ROOT=\$ANDROID_HOME
export ANDROID_NDK_HOME=$ANDROID_NDK_HOME
export JAVA_HOME=$JAVA_HOME
export PATH=\$JAVA_HOME/bin:\$HOME/.cargo/bin:\$PATH:\$ANDROID_HOME/cmdline-tools/latest/bin:\$ANDROID_HOME/platform-tools:\$ANDROID_HOME/emulator:\$HOME/.local/bin
EOF

# Reporting block: explicitly disable pipefail so the `| head -1` short-reads
# don't propagate SIGPIPE (141) from the upstream commands and fail the bake
# after all the real install work succeeded.
set +o pipefail
echo ">>> installed versions:"
xcodebuild -version | head -1
"$JAVA_HOME/bin/java" -version 2>&1 | head -1
"$SDKMANAGER" --version 2>/dev/null | head -1
"$ANDROID_HOME/platform-tools/adb" --version | head -1
"$HOME/.cargo/bin/rustc" --version 2>/dev/null | head -1
"$HOME/.cargo/bin/cargo" --version 2>/dev/null | head -1
"$HOME/.cargo/bin/cargo" ndk --version 2>/dev/null | head -1
echo "NDK: ${ANDROID_NDK_VERSION} at ${ANDROID_NDK_HOME}"
just --version 2>/dev/null | head -1
node --version 2>/dev/null || true
corepack --version 2>/dev/null || true
"$HOME/.local/bin/uv" --version 2>/dev/null || true
python3 --version 2>/dev/null || true
echo ">>> done"
