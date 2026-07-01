# bakery

Bakes Tart goldens and stamps GitHub Actions runner VMs from them — on a
single Apple Silicon Mac. One YAML describes the fleet; `bakery` clones,
boots, registers.

Goldens are the *tarts*. The CLI is the *bakery*. Runner VMs are the stamped
clones.

## Install on a new Mac

Prereqs: Apple Silicon, macOS 13+, GitHub PAT with `admin:org`.

```sh
brew install cirruslabs/cli/tart sshpass rustup
rustup-init -y

git clone git@github.com:Archdoog/bakery.git && cd bakery
cargo install --path .                   # -> ~/.cargo/bin/bakery
cp runners.yaml.example runners.yaml     # set github.org and per-class sizing
export GH_PAT=ghp_...                    # or set `token_env` in runners.yaml
```

On first `tart` run, macOS prompts for **Local Network** access. Allow it
(System Settings → Privacy & Security → Local Network), otherwise the host
can't reach VMs over vmnet and `bakery up` hangs on SSH.

## Bake goldens

Each class needs its golden built once.

```sh
bakery build ubuntu      # ~15-20 min, ~3 GB download
bakery build macos       # ~25-40 min, ~12 GB (iOS 18 runtime alone is 8 GB)
```

Add a new golden: drop `scripts/bake-<name>.sh` + `scripts/bake/<name>-install.sh`;
`bakery build <name>` picks it up.

### Bumping a tool version (e.g. Xcode)

Tool versions live in the bake scripts — Xcode comes from the Cirrus base image
(`BAKE_SOURCE` in `scripts/bake-macos.sh`, default `…macos-tahoe-xcode:latest`),
others from `scripts/bake/<name>-install.sh` (e.g. `IOS_RUNTIME_VERSION`). Pin
the version, re-bake, then re-clone the runners:

```sh
# Xcode 26.5 (Cirrus publishes a matching tag):
BAKE_SOURCE=ghcr.io/cirruslabs/macos-tahoe-xcode:26.5 tart delete macos; bakery build macos
bakery recycle macos     # re-stamp runners from the new golden
```

Edit the script default if the bump is permanent rather than a one-off.

## Up, down, status

```sh
bakery plan     # validate config, check host fit
bakery up       # clone, boot, register
bakery status   # list tracked runners
bakery down     # stop VMs, deregister
bakery recycle  # destroy + re-create from golden (wipes accumulated build cruft)
```

Targeted forms: `bakery up ubuntu` (group), `bakery up ubuntu-1` (one runner).
Same on `down` and `recycle`.

Verify on GitHub → **Org → Settings → Actions → Runners** — should show Idle.

## Disk hygiene

Self-hosted runners persist `_work/` across jobs, so build outputs, action
caches, and (on macOS) DerivedData / Simulator devices accumulate. Two
mechanisms keep that bounded:

- **Per-job hook.** Every runner has an `ACTIONS_RUNNER_HOOK_JOB_COMPLETED`
  pointing at `~/actions-runner/runner-hooks/job-completed.sh`. After each
  job it clears `_work/_temp`, prunes `_work/_actions` and `_diag` entries
  older than 7 days, and on macOS evicts the Xcode `ModuleCache.noindex`
  and stale DerivedData / Simulator devices. Tool caches (`~/.gradle`,
  `~/.cargo`, `/opt/hostedtoolcache`, Homebrew, CocoaPods) are left alone
  — those are what make a warm self-hosted runner fast.
- **`bakery recycle`** for the bulk reset. Tart clones are CoW so destroying
  + re-cloning a runner takes ~30–60s and gives you a clean VM identical to
  the golden. Run it nightly via `cron`/`launchd` when the queue is idle,
  or on demand when a runner reports disk pressure. Note the in-guest hook
  above can't return space to the *host*: once a clone diverges from its
  golden, those blocks stay allocated until the VM is re-cloned — recycle is
  what keeps the host disk bounded.
- **Tart cache prune.** The OCI/IPSW cache under `~/.tart/cache` only speeds
  up re-pulls at bake time, but a couple of image bumps can leave 60–80 GB
  behind. Set `host.cache_retention_days` in `runners.yaml` (see the example)
  and `recycle` will run `tart prune --entries caches --older-than N` while
  the fleet is down. Goldens and runner VMs are never touched.

### Nightly recycle via launchd

```sh
scripts/nightly-recycle.sh --install          # daily at 03:30
scripts/nightly-recycle.sh --install 02:00    # custom time
```

Installs a LaunchAgent that runs `bakery recycle` from this repo root,
logging to `logs/nightly-recycle.log`. launchd doesn't inherit your shell
environment, so put the PAT in `.env` at the repo root (gitignored):
`export GH_PAT=ghp_...`. Remove the agent with
`launchctl bootout gui/$(id -u)/dev.bakery.nightly-recycle`.

## Updating after `git pull`

`bakery up` is idempotent: golden-fingerprint drift triggers a re-clone;
offline GitHub registrations get pruned and re-created.

```sh
git pull
cargo install --path .              # refresh the CLI

# Re-bake any goldens whose scripts/bake/ changed:
tart delete <name> && bakery build <name>

bakery up                           # reconciles everything
```

Full reset — drop everything and re-bake from scratch:

```sh
bakery down --destroy               # also `tart delete` each child clone
tart delete macos ubuntu            # drop goldens
bakery build macos && bakery build ubuntu
bakery up
```

## Workflow labels

Each class in `runners.yaml` advertises a stable label set. Workflows pick
classes by listing the labels they need:

```yaml
jobs:
  ios:      { runs-on: [self-hosted, macos, arm64, xcode] }
  android:  { runs-on: [self-hosted, macos, arm64, android] }
  web:      { runs-on: [self-hosted, linux, arm64, node] }
  python:   { runs-on: [self-hosted, linux, arm64, python] }
  rust:     { runs-on: [self-hosted, linux, arm64, rust] }
```

## Caveats

- **2 macOS guests max** per host (Apple EULA). Linux is unbounded.
- **Android emulator on M1/M2** falls back to TCG software emulation
  (10-50× slower — no nested virtualization). Route `connectedAndroidTest`
  to `runs-on: macos-latest`, or use an M3+ host.
- **Node projects must pin `packageManager`** — ubuntu enables corepack
  instead of a global `pnpm`.

## Layout

```
src/                  Rust CLI (plan / up / down / status / build)
scripts/
  provision-*.sh      runs inside each runner on first boot
  bake-*.sh           host-side orchestrator for `bakery build <name>`
  bake/*-install.sh   runs inside the golden during bake
runners.yaml          your fleet (gitignored)
runners.yaml.example  template
state/                per-runner JSON (gitignored)
logs/                 tart stdout/stderr per VM (gitignored)
```

## Troubleshooting

- `sshpass not found` → `brew install sshpass`.
- `bakery up` hangs on SSH → Local Network permission denied for `tart`.
- `config.sh` token errors → PAT missing `admin:org`, or token >1 h old;
  rerun.
- Runner stays Offline → check `logs/<name>.log`; `tart ip <name>` + ssh
  in for live debug.
- Plan fails resource budget → shrink per-VM resources or `host.reserve`.
