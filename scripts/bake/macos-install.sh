#!/bin/bash
# Install build tools inside a disposable macOS golden.
set -euo pipefail

# Non-interactive SSH doesn't source ~/.zprofile; put Apple Silicon brew on PATH.
eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null)" || true
# The Cirrus image already has some formulae (node@24). Do not let `brew install`
# upgrade them. An upgrade pulls in new dependencies, and Homebrew 5 then can
# stop on its own lock: "A `brew install --quiet node@24` process has already
# locked /opt/homebrew/Cellar/openssl@3".
export HOMEBREW_NO_INSTALL_UPGRADE=1

ANDROID_HOME="$HOME/Library/Android/sdk"

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

echo ">>> latest Android command-line tools (Homebrew cask)"
if brew list --cask android-commandlinetools >/dev/null 2>&1; then
  brew upgrade --quiet --cask android-commandlinetools
else
  brew install --quiet --cask android-commandlinetools
fi
export ANDROID_HOME
if [[ -d "$HOME/android-sdk" && ! -e "$ANDROID_HOME" ]]; then
  mkdir -p "$(dirname "$ANDROID_HOME")"
  mv "$HOME/android-sdk" "$ANDROID_HOME"
fi
mkdir -p "$ANDROID_HOME"
# A bounded input avoids SIGPIPE from an endless yes process.
printf 'y\n%.0s' {1..50} | sdkmanager --sdk_root="$ANDROID_HOME" --licenses >/dev/null
chmod +x /tmp/bakery
/tmp/bakery install-tools /tmp/bakery-tools.json
# shellcheck source=/dev/null
source /tmp/bakery-android.env

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
sdkmanager --version 2>/dev/null | head -1
"$ANDROID_HOME/platform-tools/adb" --version | head -1
"$HOME/.cargo/bin/rustc" --version 2>/dev/null | head -1
"$HOME/.cargo/bin/cargo" --version 2>/dev/null | head -1
"$HOME/.cargo/bin/cargo" ndk --version 2>/dev/null | head -1
echo "NDK: ${ANDROID_NDK_HOME}"
just --version 2>/dev/null | head -1
node --version 2>/dev/null || true
corepack --version 2>/dev/null || true
"$HOME/.local/bin/uv" --version 2>/dev/null || true
python3 --version 2>/dev/null || true
echo ">>> done"
