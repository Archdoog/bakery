#!/bin/bash
# Runs inside a macOS aarch64 guest (cloned from macos-tahoe-xcode) during
# `bake-macos.sh`. Layers on top of the stock Cirrus Xcode image:
#   - iOS simulator runtime $IOS_RUNTIME_VERSION (default 27.0) + iPhone 17 Pro Max device
#   - iOS simulator runtime $MAESTRO_IOS_RUNTIME_VERSION (default 18.6) + iPhone 16
#     device for Maestro smoke tests
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
# The stock image ships Xcode 27 with its matching iOS 27.x runtime. We pin the
# point release so the golden does not drift with the base image. Maestro smoke
# tests run on iOS 18, so the bake also adds the last iOS 18 runtime.
#
# Note: on M1/M2 hosts the Android emulator will run in TCG (software) mode
# because there's no nested virtualization — expect 10-50x slower instrumentation
# tests than on bare metal. Full acceleration requires M3+ with tart's
# --nested-virtualization enabled.
set -euo pipefail

# Non-interactive SSH doesn't source ~/.zprofile; put Apple Silicon brew on PATH.
eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null)" || true
# The Cirrus image already has some formulae (node@24). Do not let `brew install`
# upgrade them. An upgrade pulls in new dependencies, and Homebrew 5 then can
# stop on its own lock: "A `brew install --quiet node@24` process has already
# locked /opt/homebrew/Cellar/openssl@3".
export HOMEBREW_NO_INSTALL_UPGRADE=1

IOS_RUNTIME_VERSION="${IOS_RUNTIME_VERSION:-27.0}"
MAESTRO_IOS_RUNTIME_VERSION="${MAESTRO_IOS_RUNTIME_VERSION:-18.6}"
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

# ---- iOS simulator runtimes ------------------------------------------------

echo ">>> Xcode info"
xcodebuild -version

# SSH comes up before the guest's auto-login finishes. Until then, simctl and
# xcodebuild cannot connect to the simulator services. Wait for them.
echo ">>> waiting for the simulator services"
for _ in $(seq 60); do
    xcrun simctl list runtimes >/dev/null 2>&1 && xcrun simctl runtime list >/dev/null 2>&1 && break
    sleep 5
done

echo ">>> runtimes before"
xcrun simctl list runtimes || true

# Print the identifier of the available iOS <version> runtime, or nothing.
runtime_id_for() {
    xcrun simctl list runtimes -j | /usr/bin/python3 -c "
import json, sys
version = sys.argv[1]
for r in json.load(sys.stdin)['runtimes']:
    ok = r.get('isAvailable') and r.get('platform') == 'iOS'
    if ok and r.get('version', '').startswith(version):
        print(r['identifier']); break
" "$1"
}

# Make sure that iOS <version> and device <name> of <type> are on the guest.
# If the runtime is missing, Xcode downloads it. Xcode 16+ can pin a runtime
# version with -buildVersion. The runtime ships as a signed distribution, so no
# Apple ID is necessary.
install_runtime_device() {
    local version="$1" name="$2" type="$3" runtime_id
    runtime_id=$(runtime_id_for "$version")
    if [[ -z "$runtime_id" ]]; then
        echo ">>> downloading iOS $version simulator runtime (~8GB, 15-25 min)"
        xcodebuild -downloadPlatform iOS -buildVersion "$version"
        runtime_id=$(runtime_id_for "$version")
    fi
    [[ -n "$runtime_id" ]] \
        || { echo "no iOS $version runtime found among available runtimes" >&2; exit 1; }
    echo ">>> iOS $version runtime id: $runtime_id"

    # Idempotent create — Xcode's default set or a prior bake may already have
    # this device on the runtime. Skip it then, so the name stays unambiguous.
    if ! xcrun simctl list devices -j | /usr/bin/python3 -c "
import json, sys
devices = json.load(sys.stdin)['devices'].get('${runtime_id}', [])
sys.exit(0 if any(d.get('name') == '${name}' for d in devices) else 1)
" 2>/dev/null; then
        echo ">>> creating simulator '$name' on iOS $version"
        xcrun simctl create "$name" "$type" "$runtime_id"
    else
        echo ">>> simulator '$name' already exists on iOS $version, skipping create"
    fi
}

# Xcode 27 does not download iOS 18 runtimes ("not available for download").
# bake-macos.sh copies a signed runtime image to the guest, and this step
# imports it. The image is either a raw runtime image from a host Xcode or an
# `xcodebuild -exportPath` bundle. Do not use `simctl runtime add --move`: for
# a bundle, it tries to remove the inner image from the read-only bundle volume
# and marks the runtime "Unusable". If simctl gives no usable runtime, try
# `xcodebuild -importPlatform`, which reads -exportPath bundles.
if [[ -n "${MAESTRO_IOS_RUNTIME_DMG:-}" ]]; then
    echo ">>> importing iOS $MAESTRO_IOS_RUNTIME_VERSION runtime from $MAESTRO_IOS_RUNTIME_DMG"
    xcrun simctl runtime add "$MAESTRO_IOS_RUNTIME_DMG" || true
    # The runtime can take a few seconds to show up after the add returns.
    for _ in $(seq 15); do
        [[ -n "$(runtime_id_for "$MAESTRO_IOS_RUNTIME_VERSION")" ]] && break
        sleep 2
    done
    if [[ -z "$(runtime_id_for "$MAESTRO_IOS_RUNTIME_VERSION")" ]]; then
        echo ">>> simctl import gave no usable runtime, trying xcodebuild -importPlatform"
        xcrun simctl runtime delete --unusable || true
        xcodebuild -importPlatform "$MAESTRO_IOS_RUNTIME_DMG" || true
    fi
    [[ -n "$(runtime_id_for "$MAESTRO_IOS_RUNTIME_VERSION")" ]] || {
        echo "could not import iOS $MAESTRO_IOS_RUNTIME_VERSION from $MAESTRO_IOS_RUNTIME_DMG" >&2
        xcrun simctl runtime list >&2 || true
        exit 1
    }
    rm -f "$MAESTRO_IOS_RUNTIME_DMG"
fi

# Bare name: Rallista-iOS-V3 targets "iPhone 17 Pro Max" with no OS suffix.
install_runtime_device "$IOS_RUNTIME_VERSION" "iPhone 17 Pro Max" \
    "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro-Max"
install_runtime_device "$MAESTRO_IOS_RUNTIME_VERSION" \
    "iPhone 16 (iOS ${MAESTRO_IOS_RUNTIME_VERSION})" \
    "com.apple.CoreSimulator.SimDeviceType.iPhone-16"

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
