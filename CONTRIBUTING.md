# Contributing to bakery

Thanks for taking a look. This is a small Rust CLI that orchestrates Tart VMs;
the surface area is intentionally narrow, so contributions that keep it that
way are the easiest to merge.

## Dev setup

Prereqs: Apple Silicon, macOS 13+, [Tart](https://tart.run), `sshpass`, and a
stable Rust toolchain.

```sh
brew install cirruslabs/cli/tart sshpass rustup
rustup-init -y

git clone git@github.com:Archdoog/bakery.git && cd bakery
cargo build
```

For real end-to-end testing you'll need a GitHub org and a PAT with
`admin:org`. Most code changes can be sanity-checked with `cargo check` +
`cargo clippy` without ever booting a VM.

## Tests

```sh
cargo check
cargo clippy -- -D warnings
cargo fmt -- --check
```

There aren't unit tests checked in yet — the meaningful surface is host
detection, fleet planning, and the bake/provision SSH dance, all of which are
better tested end-to-end on a real host. PRs that add focused tests for pure
logic (e.g. `host::check_fit`, label/group filtering) are very welcome.

## Adding a new golden class

A "golden" is a Tart VM image that `bakery` stamps runner clones from. To
teach `bakery` about a new class, drop two scripts:

1. `scripts/bake-<name>.sh` — host-side orchestrator. Clones a base OCI
   image, boots it, runs the installer over SSH, shuts it down. Use
   `scripts/bake-ubuntu.sh` as a template; the contract is that it accepts an
   optional first argument for the destination VM name and exits 0 on success.
2. `scripts/bake/<name>-install.sh` — runs *inside* the VM during bake.
   Installs whatever toolchain the runner class needs. Should be idempotent;
   `bakery build <name>` re-runs the whole pipeline on each invocation.

Once both exist, `bakery build <name>` will pick it up automatically (the
build command discovers goldens by globbing `scripts/bake-*.sh`).

For the runtime side, you'll also need to make sure
`scripts/provision-<kind>.sh` exists for your golden's `kind` (`linux` or
`macos`). Both currently exist — most new goldens will reuse them.

## Code style

- The CLI keeps `anyhow` at the `main.rs` boundary and `thiserror` for module
  error enums. New modules that surface errors to the caller should follow
  that split.
- One YAML schema, one binary, no plugin surface. If a change introduces a
  new abstraction layer ("strategy", "provider", etc.), call that out in the
  PR — usually we'd rather grow the YAML.
- Comments explain *why*, not *what*. The codebase is small; readers can read.

## Filing issues

- **Bug** — include `bakery plan` output, the failing command, and the tail
  of `logs/<vm>.log`.
- **Feature** — say what you want to do; we'll figure out whether it belongs
  in the CLI, the bake scripts, or a workflow.
- **New golden class** — open an issue describing the toolchain before
  writing the bake script, so we can sanity-check the class against Apple's
  EULA cap (2 macOS guests) and aarch64 toolchain gaps (no Android SDK on
  Linux ARM, etc.).

## License

By contributing you agree your changes will be released under the project's
[BSD 3-Clause license](LICENSE).
