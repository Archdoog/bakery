#!/bin/bash
# Runs inside a macOS aarch64 guest (cloned from macos-tahoe-xcode) during
# `bake-macos.sh`. Layers on top of the stock Cirrus Xcode image:
#   - Simulator runtimes and devices from runners.yaml
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

ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ANDROID_NDK_VERSION="${ANDROID_NDK_VERSION:-26.2.11394342}"
CMDLINE_TOOLS_BUILD="${CMDLINE_TOOLS_BUILD:-11076708}"

command -v xcodebuild >/dev/null \
  || { echo "xcodebuild missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }
command -v brew >/dev/null \
  || { echo "brew missing — expected on a Cirrus macos-tahoe-xcode golden" >&2; exit 1; }

# Prebuilt Ruby extensions use absolute Homebrew library paths.
echo ">>> Ruby runtime libraries"
brew install --quiet openssl@3 openssl@4 libyaml gmp

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

chmod +x /tmp/bakery
/tmp/bakery simulators install /tmp/bakery-simulators.json

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
