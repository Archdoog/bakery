# Apple toolchains

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

## Rebuild a golden

Stop the watcher before a bake. Wait until the runners are idle.

1. Set the Xcode versions and simulator targets in `runners.yaml`.
2. Delete the affected golden with `tart delete macos`.
3. Rebuild it with `bakery build macos`.
4. Replace the existing runners with `bakery recycle macos`.
5. Restart the watcher with `bakery watch`.

The bake installs these tools once in the golden. Ephemeral runner replacement
clones that golden for each job. Runner provisioning does not install Xcode or
simulator runtimes.
