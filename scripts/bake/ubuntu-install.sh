#!/bin/bash
# Runs inside an Ubuntu aarch64 guest during `bake-ubuntu.sh`. Installs:
#   - Temurin JDK 21 (apt) — general JVM work; not for Android builds
#   - Node.js 24 LTS + corepack (pnpm/yarn resolved per-repo via packageManager)
#   - uv (Astral Python installer) + python-is-python3 symlink
#   - rustup + stable toolchain (default profile: rustfmt + clippy included)
#   - just (command runner) — prebuilt aarch64 binary, dropped into ~/.cargo/bin
#   - Common native build deps: cmake, protobuf-compiler, libclang-dev
# Writes toolchain paths into ~/.bashrc; actions-runner reads a separate `.env`
# written by scripts/provision-linux.sh.
#
# Android is intentionally not here: Google's aapt2 ships only as x86_64 ELF
# (running it on aarch64 needs qemu-user-static + amd64 multi-arch libs) and
# Paparazzi's layoutlib-runtime has no linux-aarch64 JNI variant at all. Both
# dead-ends route cleanly through the macOS ARM golden instead (bake-macos.sh).
set -euo pipefail

echo ">>> apt base packages"
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  apt-transport-https build-essential ca-certificates cmake curl git gpg \
  libclang-dev libssl-dev lsb-release pkg-config protobuf-compiler \
  python-is-python3 unzip wget zip

echo ">>> Temurin JDK 21 (Adoptium apt repo)"
wget -qO - https://packages.adoptium.net/artifactory/api/gpg/key/public \
  | sudo gpg --dearmor -o /usr/share/keyrings/adoptium.gpg
echo "deb [signed-by=/usr/share/keyrings/adoptium.gpg] https://packages.adoptium.net/artifactory/deb $(lsb_release -cs) main" \
  | sudo tee /etc/apt/sources.list.d/adoptium.list >/dev/null
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq temurin-21-jdk

echo ">>> Node.js 24 LTS + corepack"
curl -fsSL https://deb.nodesource.com/setup_24.x | sudo -E bash - >/dev/null
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nodejs
# corepack ships with Node 24 but is disabled by default. Enabling it lets
# each project pull the pnpm/yarn version pinned in its package.json's
# `packageManager` field — no global install, no version drift across repos.
sudo corepack enable

echo ">>> uv (Astral Python)"
curl -LsSf https://astral.sh/uv/install.sh | sh >/dev/null

echo ">>> rustup + stable (default profile: rustfmt + clippy)"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --default-toolchain stable --profile default >/dev/null

echo ">>> just (prebuilt aarch64 binary)"
# Upstream installer picks the right target; --to a PATH-included dir.
curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh \
  | bash -s -- --to "$HOME/.cargo/bin" >/dev/null

# GitHub-hosted images ship /opt/hostedtoolcache owned by the runner user;
# setup-ruby/setup-go/setup-python hardcode that path and EACCES on a vanilla
# /opt. Pre-create it so tool-cache downloads work on first job.
echo ">>> /opt/hostedtoolcache (runner-owned)"
sudo mkdir -p /opt/hostedtoolcache
sudo chown "$(id -u):$(id -g)" /opt/hostedtoolcache

echo ">>> shell profile"
cat <<EOF >> "$HOME/.bashrc"

# --- bakery golden toolchain ---
# corepack's download prompt defaults to y/n interactive; non-interactive CI
# shells never answer and hang until GHA times out. Force auto-proceed.
export COREPACK_ENABLE_DOWNLOAD_PROMPT=0
export PATH=\$PATH:\$HOME/.cargo/bin:\$HOME/.local/bin
EOF

echo ">>> apt cleanup"
sudo apt-get clean
sudo rm -rf /var/lib/apt/lists/*

echo ">>> installed versions:"
java -version 2>&1 | head -1
node --version
corepack --version 2>/dev/null || true
python3 --version
"$HOME/.cargo/bin/rustc" --version 2>/dev/null || true
"$HOME/.cargo/bin/cargo" fmt --version 2>/dev/null || true
"$HOME/.cargo/bin/cargo" clippy --version 2>/dev/null || true
"$HOME/.cargo/bin/just" --version 2>/dev/null || true
"$HOME/.local/bin/uv" --version 2>/dev/null || true
echo ">>> done"
