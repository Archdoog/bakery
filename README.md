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
bakery build macos       # downloads depend on the selected Xcode versions and runtimes
```

The macOS bake reads Xcode and simulator settings from `goldens.<destination>`
in `runners.yaml`. The destination is `macos`, or the name passed with `--as`.
The example preserves the iOS 27.0 and iOS 18.6 devices used by existing jobs.

```yaml
goldens:
  macos:
    xcodes:
      - version: "27"
      - version: "26.5"
    default_xcode: "27"
    simulators:
      - platform: iOS
        version: "27.0"
        xcode: "27"
        devices:
          - name: iPhone 17 Pro Max
            device_type: com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro-Max
```

Each `xcodes` entry selects one Xcode version. The bake installs each version
at `/Applications/Xcode-<version>.app`. `default_xcode` selects the version for
runner jobs. If omitted, the first entry is the default. Jobs can select another
installed version with `DEVELOPER_DIR`:

```yaml
env:
  DEVELOPER_DIR: /Applications/Xcode-26.5.app/Contents/Developer
```

The bake uses the first Xcode entry's source image as its base. By default,
that source is `ghcr.io/cirruslabs/macos-tahoe-xcode:<version>`.
Set an entry's `source` to use another Tart image. Set `goldens.<destination>.source`
to select the golden's macOS base independently. An explicit `BAKE_SOURCE`
environment variable overrides the YAML base source.

For each missing Xcode version, the bake starts a temporary guest from that
entry's source image. It copies Xcode into `state/xcode-<version>.tar` and then
into the golden. Later bakes reuse the archive. The donor and golden run
separately. The bake checks each app's version before it selects the default.

No Apple ID sign-in is necessary. Each selected Xcode version must support the
golden's macOS version.

Quote version numbers. A selector such as `27` accepts an installed `27.x`
release. Use a full version and a matching source tag to pin a release.
Version selectors must not overlap. If `xcodes` is empty or omitted, the bake
keeps the base image's Xcode installation and default selection.

Each simulator entry can set `xcode` to a configured Xcode version. The bake
uses that version for runtime installation and device creation. If omitted,
the bake uses the default Xcode. Installed Xcode versions share simulator runtime
files. Each Xcode must support the runtime it uses.

Each entry installs one platform version. Supported platforms are `iOS`,
`watchOS`, `tvOS`, and `visionOS`. Quote each version. Omit `devices` to install
only the runtime.

Use `simulators: []` for a golden without simulators.
If the golden has no configuration entry, the bake uses an empty simulator list.
The bake deletes unselected devices and runtimes inherited from the base image.
It does this inside the golden, before runner provisioning.

Devices share runtime files. Removing a runtime saves more space than removing
an unused device. Downloads request the Apple Silicon runtime variant.
Use `xcrun simctl list devicetypes` to find device type identifiers.

To install iOS 18.6 with the configured Xcode 26.5, add another simulator entry:

```yaml
      - platform: iOS
        version: "18.6"
        xcode: "26.5"
        devices:
          - name: iPhone 16 (iOS 18.6)
            device_type: com.apple.CoreSimulator.SimDeviceType.iPhone-16
```

To use an external runtime image, set `runtime_image` to a host image path.
Alternatively, set `donor_source` to a compatible Tart image. This avoids
installing another Xcode version in the golden:

```yaml
      - platform: iOS
        version: "18.6"
        donor_source: ghcr.io/cirruslabs/macos-tahoe-xcode:26.5
        devices:
          - name: iPhone 16 (iOS 18.6)
            device_type: com.apple.CoreSimulator.SimDeviceType.iPhone-16
```

If the golden already has the runtime, the bake skips the image transfer.
Otherwise, the bake first checks `runtime_image`, then `state/<platform>-<version>-simulator-runtime.dmg`,
then the host's installed runtime. The cache uses lowercase platform names.
Paths are relative to the repository root.

If none exists, `donor_source` starts a temporary guest to download the image.
The bake caches the image in `state/` and deletes the donor guest and its added
OCI cache entries. Without `donor_source`, the golden downloads the runtime itself.
The donor and golden run separately.

Existing configurations must add `goldens.macos.simulators` before the next macOS bake.
The bake does not use `IOS_RUNTIME_VERSION`, `MAESTRO_IOS_RUNTIME_VERSION`,
`MAESTRO_IOS_RUNTIME_DMG`, or `RUNTIME_DONOR_SOURCE`.

Add a new golden: drop `scripts/bake-<name>.sh` + `scripts/bake/<name>-install.sh`;
`bakery build <name>` picks it up.

### Bumping a tool version (e.g. Xcode)

Set Xcode versions, the default Xcode, and simulator targets in `runners.yaml`.
Other tools come from `scripts/bake/<name>-install.sh`.
Then rebuild the golden and replace its runners:

```sh
tart delete macos
bakery build macos
bakery recycle macos
```

The macOS build requires the configuration file. `bakery build` still lists
available bake scripts without a configuration file. Ubuntu builds do not require one.

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
  or on demand when a runner reports disk pressure.

The host fills up too. A runner VM is an APFS clone, so it does not need the
tart OCI cache or its golden after `up`. Use `bakery clean` to free that
space:

```sh
bakery clean            # delete the tart OCI and IPSW caches, keep the goldens
bakery clean --goldens  # also delete the goldens, on a host that is short of disk
```

- After `clean`, the next `bakery build` downloads its base image again.
- After `clean --goldens`, do a `bakery build` before the next `recycle` or
  new runner.
- The clone, the golden, and the cache share disk blocks. Thus `clean` frees
  less than the sizes in `tart list` show. It prints the real amount.

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
src/                  Rust CLI (plan / up / down / recycle / status / build / clean)
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
