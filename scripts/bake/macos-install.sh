#!/bin/bash
# Runs inside a macOS aarch64 guest (cloned from macos-tahoe-xcode) during
# `bake-macos.sh`. Layers on top of the stock Cirrus Xcode image:
#   - iOS simulator runtime $IOS_RUNTIME_VERSION (default 18.5) + iPhone 16 device
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
ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ANDROID_NDK_VERSION="${ANDROID_NDK_VERSION:-26.2.11394342}"
CMDLINE_TOOLS_BUILD="${CMDLINE_TOOLS_BUILD:-11076708}"

command -v xcodebuild >/dev/null \
  || { echo "xcodebuild missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }
command -v brew >/dev/null \
  || { echo "brew missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }

# ---- iOS simulator runtime -------------------------------------------------

echo ">>> Xcode info"
xcodebuild -version

echo ">>> runtimes before"
xcrun simctl list runtimes || true

echo ">>> downloading iOS $IOS_RUNTIME_VERSION simulator runtime (~8GB, 15-25 min)"
# Xcode 16+ supports pinning a specific version via -buildVersion. The runtime
# ships as a signed distribution, no Apple ID needed.
xcodebuild -downloadPlatform iOS -buildVersion "$IOS_RUNTIME_VERSION"

echo ">>> runtimes after"
xcrun simctl list runtimes

RUNTIME_ID=$(xcrun simctl list runtimes -j | /usr/bin/python3 -c "
import json, sys
runtimes = json.load(sys.stdin)['runtimes']
match = next(
    (r for r in runtimes
     if r.get('isAvailable')
     and r.get('platform') == 'iOS'
     and r.get('version', '').startswith('${IOS_RUNTIME_VERSION}')),
    None,
)
if match is None:
    sys.exit(f'no iOS ${IOS_RUNTIME_VERSION} runtime found among available runtimes')
print(match['identifier'])
")
echo ">>> runtime id: $RUNTIME_ID"

DEVICE_NAME="iPhone 16 (iOS ${IOS_RUNTIME_VERSION})"
DEVICE_TYPE="com.apple.CoreSimulator.SimDeviceType.iPhone-16"

# Idempotent create — if a prior bake ran the script, skip rather than duplicate.
if ! xcrun simctl list devices -j | /usr/bin/python3 -c "
import json, sys
devices = json.load(sys.stdin)['devices']
name = '${DEVICE_NAME}'
sys.exit(0 if any(d.get('name') == name for v in devices.values() for d in v) else 1)
" 2>/dev/null; then
    echo ">>> creating simulator '$DEVICE_NAME'"
    xcrun simctl create "$DEVICE_NAME" "$DEVICE_TYPE" "$RUNTIME_ID"
else
    echo ">>> simulator '$DEVICE_NAME' already exists, skipping create"
fi

echo ">>> available devices on iOS $IOS_RUNTIME_VERSION:"
xcrun simctl list devices available | grep -A1 "iOS ${IOS_RUNTIME_VERSION}" || true

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
