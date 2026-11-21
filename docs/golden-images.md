# Golden images

Set `root_images` in `runners.yaml` to choose the root image for each bake script:

```yaml
root_images:
  macos: ghcr.io/cirruslabs/macos-golden-gate-xcode
  ubuntu: ghcr.io/cirruslabs/ubuntu
```

Both references use `:latest`. These are also the defaults when a root entry is absent.
Use a tag or digest to pin a remote image. Local Tart image names remain unchanged.
Registry ports do not count as image tags.

Root image selection is independent of the configured Xcode versions. A macOS
bake installs any requested Xcode version that the root does not contain.
See [Apple toolchains](apple-toolchains.md) for Xcode and simulator settings.

To override the root for one golden, set its `source`:

```yaml
root_images:
  ubuntu: ghcr.io/cirruslabs/ubuntu

goldens:
  ubuntu-ci:
    source: ghcr.io/example/ubuntu-ci:24.04
```

Run `bakery build ubuntu --as ubuntu-ci` to use that override.
The same rule applies to macOS goldens. The destination selects the golden
entry, while the bake script name selects the shared root.

The selection order is:

1. The `BAKE_SOURCE` environment variable.
2. The destination golden's `source`.
3. The bake script's entry in `root_images`.
4. The built-in root for `macos` or `ubuntu`.

Ubuntu builds read the configuration file when it exists. Without one, they use
the default root. macOS builds require the configuration file for tool selection.
`bakery build` still lists scripts without a configuration file.

Stop the watcher before rebuilding a golden. Wait until its runners are idle.
After rebuilding, recycle the affected runners and restart the watcher.
Ephemeral replacements inherit the root and tools from the rebuilt golden.
