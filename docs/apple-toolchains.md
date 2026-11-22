# macOS build tools

With no tool settings, `bakery build macos` uses the latest macOS root and Xcode.
It installs the latest stable Android platform, build tools, platform tools, and NDK.
It removes inherited simulator runtimes, simulator devices, Android system images, emulators, and AVDs.
The build prints warnings for empty simulator and emulator selections.
Teams can install extra tools in CI scripts.

Android tools run on the macOS golden. The Ubuntu golden does not include Android tools.

Optional settings belong to `goldens.<destination>` in `runners.yaml`.
The destination defaults to `macos`. Use `--as` to select another name.

```yaml
goldens:
  macos:
    xcodes: ["27.2", "26.5"]
    simulators:
      - version: "27.2"
        devices: ["iPhone 17 Pro Max"]
    android_packages:
      - emulator
      - system-images;android-37;google_apis;arm64-v8a
```

`xcodes` overrides the default with exact versions. The highest version becomes active.

Jobs can select another installed version with `DEVELOPER_DIR=/Applications/Xcode-26.5.app/Contents/Developer`.
Missing versions come from `ghcr.io/cirruslabs/macos-tahoe-xcode:<version>` guests.

The bake caches their archives under `state/`. Each version must support the golden's macOS.
A custom root with no Xcode override gets Xcode from the latest default root.
See [Golden images](golden-images.md) for root selection.

Each simulator entry selects an iOS runtime. Device names come from that runtime's catalog.

Omit `devices` to install only the runtime. Omit `simulators` to install neither runtimes nor devices.

The active Xcode downloads runtimes through `xcodebuild`. It must support each selected runtime.
The bake validates devices and downloads before it removes inherited simulator assets.
It does not import external runtime images or use a separate runtime donor.

`android_packages` adds SDK package identifiers. Omission installs no emulator or system image.
Use CI scripts to create AVDs when required.

To rebuild, stop the watcher and wait until the runners are idle.

1. Stop the runners with `bakery down` to free the macOS guest slots.
2. Delete the affected golden with `tart delete macos`.
3. Rebuild it with `bakery build macos`.
4. Start the runners with `bakery up`.
5. Restart the watcher with `bakery watch`.

Ephemeral runners clone the tools from the golden. Runner provisioning does not install them.
