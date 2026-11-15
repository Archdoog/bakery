# bakery

Build Tart golden images and run GitHub Actions runner VMs on one Apple Silicon Mac.

## Install and start

Requires macOS 13+ and a GitHub PAT with `admin:org` scope.
Run commands from the repository root.

```sh
brew install cirruslabs/cli/tart sshpass rustup
rustup-init -y

git clone git@github.com:Archdoog/bakery.git && cd bakery
cargo install --path .                   # Install to ~/.cargo/bin/bakery.
cp runners.yaml.example runners.yaml     # Set github.org and per-group resources.
export GH_PAT=ghp_...                    # Or set token_env in runners.yaml.
```

Edit `runners.yaml` for your organization and host resources.
Allow Tart Local Network access when macOS asks.

```sh
bakery plan
bakery build ubuntu
bakery build macos
bakery watch
```

`watch` maintains ephemeral runners and checks every 15 seconds (`--interval` changes this).
Use `bakery watch --force` to proceed when the fleet exceeds the host budget.
After each job, it saves diagnostics under `logs/` and replaces the VM from its golden.
Keep the watcher active, or run it through a service manager.
Keep the golden images available. Delete old diagnostic archives as needed.

## Update, reset, and stop

Stop the watcher before fleet changes. Wait until runners are idle before you recycle them.

```sh
git pull
cargo install --path .
bakery recycle
bakery watch
```

If bake scripts change, delete and rebuild the affected golden before `recycle`.
For example, run `tart delete ubuntu`, then `bakery build ubuntu`.

```sh
bakery status
bakery down --destroy
bakery clean
```

Stop the watcher before `down`. `clean` deletes Tart caches and keeps the goldens.
`clean --goldens` also deletes the goldens. Rebuild them before the next runner replacement.

`up`, `down`, `recycle`, and `watch` accept group or runner names, such as `ubuntu` or `ubuntu-1`.
For persistent groups (`ephemeral: false`), use `bakery up` and recycle when needed.
`up` alone does not replace runners after jobs. Use `bakery --help` for all commands.

## Fleet configuration

The example enables a fresh VM for every job on both runner groups.

```yaml
github:
  org: my-org
  token_env: GH_PAT

host:
  reserve:
    cpu: 2
    memory_gb: 8

runners:
  - name: macos
    kind: macos
    image: macos
    count: 2
    ephemeral: true
    resources:
      cpu: 3
      memory_gb: 10
      disk_gb: 180
    labels: [self-hosted, macos, arm64, xcode, android]

  - name: ubuntu
    kind: linux
    image: ubuntu
    count: 1
    ephemeral: true
    resources:
      cpu: 2
      memory_gb: 4
      disk_gb: 40
    labels: [self-hosted, linux, arm64, node, python, rust]
```

| Setting | Meaning |
| --- | --- |
| `github.org` | Organization that owns the runners. |
| `github.token_env` | Environment variable with the PAT. Defaults to `GH_PAT`. |
| `host.reserve` | CPU cores and memory for the host. Defaults to 4 cores and 8 GB. |
| `runners[].name` | Group name. Bakery names its VMs `<name>-1`, `<name>-2`, and so on. |
| `kind` | Guest platform: `macos` or `linux`. Selects the provisioning script. |
| `image` | Golden image to clone. Local goldens come from `bakery build <image>`. |
| `count` | Number of runners in the group. A host supports at most two macOS guests. |
| `ephemeral` | `true` limits each runner to one job. `watch` replaces it afterward. Omission defaults to `false`. |
| `resources.cpu` / `memory_gb` | Cores and GB per VM. `plan` checks the fleet against the host budget. |
| `resources.disk_gb` | Minimum disk size in GB. Zero or omission keeps the image size. Tart can grow disks only. |
| `labels` | Labels for workflow `runs-on` selection, such as `[self-hosted, macos, arm64, xcode]`. |
| `runner_version` | Optional top-level Actions runner version. Defaults to `2.329.0`. |

The macOS golden includes Xcode and Android tools.
Bake options, including simulator runtime sources, appear in `scripts/bake-macos.sh`.
See [CONTRIBUTING.md](CONTRIBUTING.md) for development and additional golden images.
