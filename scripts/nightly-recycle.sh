#!/usr/bin/env bash
# Nightly maintenance: `bakery recycle` — re-clones every runner from its
# golden (reclaiming CoW divergence on the host) and, when
# `host.cache_retention_days` is set in runners.yaml, prunes stale tart
# OCI/IPSW cache entries.
#
# One-time setup, run from the repo root on the runner host:
#   scripts/nightly-recycle.sh --install          # daily at 03:30
#   scripts/nightly-recycle.sh --install 02:00    # custom HH:MM
#
# launchd jobs don't inherit your shell environment, so put the GitHub PAT
# in <repo>/.env (gitignored), e.g.:  export GH_PAT=ghp_...
# Remove with: launchctl bootout gui/$(id -u)/dev.bakery.nightly-recycle
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="dev.bakery.nightly-recycle"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

if [[ "${1:-}" == "--install" ]]; then
  TIME="${2:-03:30}"
  if [[ ! "$TIME" =~ ^([01][0-9]|2[0-3]):[0-5][0-9]$ ]]; then
    echo "invalid time ${TIME@Q}; expected HH:MM (24h)" >&2
    exit 1
  fi
  HOUR=$((10#${TIME%%:*}))
  MINUTE=$((10#${TIME##*:}))
  mkdir -p "$HOME/Library/LaunchAgents" "$REPO_ROOT/logs"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$REPO_ROOT/scripts/nightly-recycle.sh</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key><integer>$HOUR</integer>
    <key>Minute</key><integer>$MINUTE</integer>
  </dict>
  <key>StandardOutPath</key><string>$REPO_ROOT/logs/nightly-recycle.log</string>
  <key>StandardErrorPath</key><string>$REPO_ROOT/logs/nightly-recycle.log</string>
</dict>
</plist>
EOF
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  echo "installed $LABEL: daily at $TIME, log at logs/nightly-recycle.log"
  exit 0
fi

# Bare launchd environment: find tart (homebrew) and bakery (cargo install).
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
cd "$REPO_ROOT"
# shellcheck disable=SC1091
[[ -f .env ]] && source .env

echo "=== $(date '+%Y-%m-%dT%H:%M:%S%z') nightly recycle ==="
exec bakery recycle
