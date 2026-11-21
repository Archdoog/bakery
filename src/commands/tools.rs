//! Prepare and install the tools selected for a macOS golden.

use crate::{
    models::fleet::{default_root, GoldenConfig, SimulatorConfig},
    paths, provision,
    runner::{GUEST_PASS, GUEST_USER},
    tart,
};
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
    time::Duration,
};

fn checked(command: &mut Command) -> Result<String> {
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .context("Cannot start tool")?;
    ensure!(
        output.status.success(),
        "Tool failed: {command:?} ({})",
        output.status
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn command(program: &str, args: &[&str]) -> Result<String> {
    checked(Command::new(program).args(args))
}

type Run<'a> = dyn FnMut(&str, &[&str]) -> Result<String> + 'a;

fn ssh(host: &str, script: &str) -> Result<String> {
    checked(
        provision::guest_command("ssh", GUEST_PASS).args([&format!("{GUEST_USER}@{host}"), script]),
    )
}

fn scp(source: &str, destination: &str) -> Result<String> {
    checked(provision::guest_command("scp", GUEST_PASS).args([source, destination]))
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn active_version(host: &str) -> Result<Option<String>> {
    let output = ssh(host, "if xcode-select -p 2>/dev/null | grep -q '/Contents/Developer$'; then xcodebuild -version; fi")?;
    if output.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(
        output
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("Xcode "))
            .context("Cannot read the Xcode version")?
            .to_owned(),
    ))
}

/// Copy missing Xcode archives to `host` using the selected `manifest` and `root` image.
///
/// # Errors
/// Return an error if an image, archive, or guest command fails.
pub(super) fn prepare(host: &str, manifest: &Path, root: &str) -> Result<()> {
    let mut golden: GoldenConfig = serde_json::from_slice(&fs::read(manifest)?)?;
    let active = active_version(host)?.unwrap_or_default();
    let latest =
        golden.xcodes.is_empty() && (active.is_empty() || Some(root) != default_root("macos"));
    let versions = if latest {
        vec!["latest".to_owned()]
    } else {
        golden.xcodes.clone()
    };
    fs::create_dir_all(paths::state_dir())?;
    for version in versions {
        if version == active {
            continue;
        }
        let archive = paths::state_dir().join(format!("xcode-{version}.tar"));
        let mut resolved = version.clone();
        if version == "latest" || !archive.is_file() {
            let donor = std::env::var("BAKERY_XCODE_DONOR")
                .unwrap_or_else(|_| format!("bakery-xcode-donor-{}", std::process::id()));
            let source = if version == "latest" {
                default_root("macos").unwrap().to_owned()
            } else {
                format!("ghcr.io/cirruslabs/macos-tahoe-xcode:{version}")
            };
            tart::clone(&source, &donor)?;
            let result = (|| -> Result<()> {
                tart::start(&donor, &paths::logs_dir().join(format!("{donor}.log")))?;
                let ip = tart::ip(&donor, 180)?;
                provision::wait_for_ssh(&ip, GUEST_USER, GUEST_PASS, 300)?;
                resolved = active_version(&ip)?.context("Donor has no active Xcode")?;
                ensure!(
                    version == "latest" || resolved == version,
                    "Donor contains Xcode {resolved}, expected {version}"
                );
                if resolved != active {
                    ssh(
                        &ip,
                        "tar -C \"$(xcode-select -p)/../..\" -cf /tmp/bakery-xcode.tar .",
                    )?;
                    let partial = archive.with_extension("tar.part");
                    scp(
                        &format!("{GUEST_USER}@{ip}:/tmp/bakery-xcode.tar"),
                        partial.to_str().context("Invalid archive path")?,
                    )?;
                    fs::rename(partial, &archive)?;
                }
                Ok(())
            })();
            tart::stop(&donor);
            tart::delete(&donor);
            result?;
        }
        if resolved != active {
            scp(
                archive.to_str().context("Invalid archive path")?,
                &format!("{GUEST_USER}@{host}:/tmp/bakery-xcode-{resolved}.tar"),
            )?;
        }
        if latest {
            golden.xcodes = vec![resolved];
        }
    }
    fs::write(manifest, serde_json::to_vec(&golden)?)?;
    Ok(())
}

fn app_version(app: &Path, run: &mut Run<'_>) -> Result<String> {
    Ok(run(
        "plutil",
        &[
            "-extract",
            "CFBundleShortVersionString",
            "raw",
            "-o",
            "-",
            app.join("Contents/Info.plist")
                .to_str()
                .context("Invalid Xcode path")?,
        ],
    )?
    .trim()
    .to_owned())
}

fn install_xcodes(
    golden: &GoldenConfig,
    apps: &Path,
    archives: &Path,
    run: &mut Run<'_>,
) -> Result<()> {
    let mut versions = golden.xcodes.clone();
    versions.sort_by_key(|version| version_key(version));
    for version in versions {
        let dest = apps.join(format!("Xcode-{version}.app"));
        let existing = fs::read_dir(apps)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .find_map(|app| {
                if !app.join("Contents/Developer/usr/bin/xcodebuild").is_file() {
                    return None;
                }
                match app_version(&app, run) {
                    Ok(actual) if actual == version => Some(Ok(app)),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?;
        if let Some(app) = existing {
            if app != dest {
                ensure!(
                    !dest.exists(),
                    "Xcode destination already exists: {}",
                    dest.display()
                );
                run(
                    "sudo",
                    &[
                        "mv",
                        app.to_str().context("Invalid Xcode path")?,
                        dest.to_str().context("Invalid Xcode path")?,
                    ],
                )?;
            }
        } else {
            ensure!(
                !dest.exists(),
                "Xcode destination already exists: {}",
                dest.display()
            );
            let archive = archives.join(format!("bakery-xcode-{version}.tar"));
            let dest = dest.to_str().context("Invalid Xcode path")?;
            run("sudo", &["mkdir", dest])?;
            run(
                "sudo",
                &[
                    "tar",
                    "-C",
                    dest,
                    "-xf",
                    archive.to_str().context("Invalid archive path")?,
                ],
            )?;
            ensure!(
                app_version(Path::new(dest), run)? == version,
                "Archive does not contain Xcode {version}"
            );
            fs::remove_file(archive)?;
        }
        let developer = dest.join("Contents/Developer");
        run(
            "sudo",
            &[
                "xcode-select",
                "--switch",
                developer.to_str().context("Invalid Xcode path")?,
            ],
        )?;
        run("sudo", &["xcodebuild", "-license", "accept"])?;
        run("sudo", &["xcodebuild", "-runFirstLaunch"])?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct Catalog {
    runtimes: Vec<Runtime>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    version: String,
    buildversion: String,
    is_available: bool,
    #[serde(default)]
    supported_device_types: Vec<DeviceType>,
}
#[derive(Deserialize)]
struct DeviceType {
    identifier: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeImage {
    build: String,
}

fn simctl(run: &mut Run<'_>, args: &[&str]) -> Result<String> {
    run("xcrun", &[&["simctl"], args].concat())
}
fn runtimes(run: &mut Run<'_>) -> Result<Vec<Runtime>> {
    Ok(serde_json::from_str::<Catalog>(&simctl(run, &["list", "runtimes", "-j"])?)?.runtimes)
}
fn matches(runtime: &Runtime, spec: &SimulatorConfig) -> bool {
    runtime.is_available
        && runtime
            .identifier
            .starts_with("com.apple.CoreSimulator.SimRuntime.iOS-")
        && runtime.version == spec.version
}

fn install_simulators(specs: &[SimulatorConfig], run: &mut Run<'_>) -> Result<()> {
    let mut catalog = runtimes(run)?;
    for spec in specs {
        if !catalog.iter().any(|runtime| matches(runtime, spec)) {
            run(
                "xcodebuild",
                &[
                    "-downloadPlatform",
                    "iOS",
                    "-buildVersion",
                    &spec.version,
                    "-architectureVariant",
                    "arm64",
                ],
            )?;
            for attempt in 0..15 {
                catalog = runtimes(run)?;
                if catalog.iter().any(|runtime| matches(runtime, spec)) {
                    break;
                }
                if attempt < 14 {
                    sleep(Duration::from_secs(2));
                }
            }
        }
    }
    let mut builds = BTreeSet::new();
    let mut devices = Vec::new();
    for spec in specs {
        let candidates: Vec<_> = catalog
            .iter()
            .filter(|runtime| matches(runtime, spec))
            .collect();
        ensure!(
            !candidates.is_empty(),
            "iOS {} is unavailable after download",
            spec.version
        );
        if spec.devices.is_empty() {
            builds.extend(
                candidates
                    .iter()
                    .map(|runtime| runtime.buildversion.clone()),
            );
        }
        for name in &spec.devices {
            let (runtime, device) = candidates
                .iter()
                .find_map(|runtime| {
                    runtime
                        .supported_device_types
                        .iter()
                        .find(|device| &device.name == name)
                        .map(|device| (*runtime, device))
                })
                .with_context(|| {
                    format!("Device {name:?} does not support iOS {}", spec.version)
                })?;
            builds.insert(runtime.buildversion.clone());
            devices.push((name, device.identifier.clone(), runtime.identifier.clone()));
        }
    }
    let images: BTreeMap<String, RuntimeImage> =
        serde_json::from_str(&simctl(run, &["runtime", "list", "-j"])?)?;
    simctl(run, &["shutdown", "all"])?;
    simctl(run, &["delete", "all"])?;
    for (id, image) in images {
        if !builds.contains(&image.build) {
            simctl(run, &["runtime", "delete", &id])?;
        }
    }
    for (name, device, runtime) in devices {
        simctl(run, &["create", name, &device, &runtime])?;
    }
    Ok(())
}

fn latest_package(list: &str, prefix: &str) -> Result<String> {
    list.lines()
        .filter_map(|line| line.split('|').next())
        .map(str::trim)
        .filter_map(|package| {
            package
                .strip_prefix(prefix)
                .map(|version| (package, version))
        })
        .filter(|(_, version)| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .max_by_key(|(_, version)| version_key(version))
        .map(|(package, _)| package.to_owned())
        .with_context(|| format!("No stable Android package found for {prefix}"))
}

fn install_android(golden: &GoldenConfig, home: &str, run: &mut Run<'_>) -> Result<String> {
    let sdk = format!("{home}/Library/Android/sdk");
    let root = format!("--sdk_root={sdk}");
    let list = run("sdkmanager", &[&root, "--list", "--channel=0"])?;
    let ndk = latest_package(&list, "ndk;")?;
    let mut packages = vec![
        "cmdline-tools;latest".to_owned(),
        "platform-tools".to_owned(),
        latest_package(&list, "platforms;android-")?,
        latest_package(&list, "build-tools;")?,
        ndk.clone(),
    ];
    packages.extend(golden.android_packages.iter().cloned());
    if packages
        .iter()
        .any(|package| package.starts_with("system-images;"))
        && !packages.iter().any(|package| package == "emulator")
    {
        packages.push("emulator".to_owned());
    }
    let args: Vec<_> = [&root, "--install", "--channel=0"]
        .into_iter()
        .chain(packages.iter().map(String::as_str))
        .collect();
    run("sdkmanager", &args)?;
    let installed = run("sdkmanager", &[&root, "--list_installed"])?;
    for package in installed
        .lines()
        .filter_map(|line| line.split('|').next())
        .map(str::trim)
    {
        if (package == "emulator"
            || [
                "system-images;",
                "platforms;",
                "build-tools;",
                "ndk;",
                "cmdline-tools;",
            ]
            .iter()
            .any(|prefix| package.starts_with(prefix)))
            && !packages.iter().any(|selected| selected == package)
        {
            run("sdkmanager", &[&root, "--uninstall", package])?;
        }
    }
    if !packages
        .iter()
        .any(|package| package == "emulator" || package.starts_with("system-images;"))
    {
        let avds = Path::new(&home).join(".android/avd");
        if avds.exists() {
            fs::remove_dir_all(avds)?;
        }
    }
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    Ok(format!("export ANDROID_HOME={}\nexport ANDROID_SDK_ROOT=\"$ANDROID_HOME\"\nexport ANDROID_NDK_HOME={}\n", quote(&sdk), quote(&format!("{sdk}/ndk/{}", ndk.trim_start_matches("ndk;")))))
}

/// Install the `manifest` tools inside a disposable macOS golden.
///
/// # Errors
/// Return an error if configuration, catalogs, or tool installation fails.
pub(super) fn install(manifest: &Path) -> Result<()> {
    let golden: GoldenConfig = serde_json::from_slice(&fs::read(manifest)?)?;
    install_xcodes(
        &golden,
        Path::new("/Applications"),
        Path::new("/tmp"),
        &mut command,
    )?;
    let mut ready = false;
    for _ in 0..60 {
        if runtimes(&mut command).is_ok() {
            ready = true;
            break;
        }
        sleep(Duration::from_secs(5));
    }
    if !ready {
        bail!("Simulator services did not respond after 5 minutes");
    }
    install_simulators(&golden.simulators, &mut command)?;
    let environment = install_android(&golden, &std::env::var("HOME")?, &mut command)?;
    fs::write("/tmp/bakery-android.env", environment)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runtime_catalog() -> String {
        json!({"runtimes": [{
            "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-2",
            "version": "27.2", "buildversion": "24C1", "isAvailable": true,
            "supportedDeviceTypes": [{"identifier": "phone", "name": "iPhone 17"}]
        }]})
        .to_string()
    }

    #[test]
    fn empty_selection_deletes_inherited_devices_and_all_runtime_images() {
        let mut calls = Vec::new();
        install_simulators(&[], &mut |program, args| {
            calls.push((program.to_owned(), args.join(" ")));
            Ok(match args {
                ["simctl", "list", "runtimes", "-j"] => runtime_catalog(),
                ["simctl", "runtime", "list", "-j"] => {
                    json!({"image": {"build": "24C1"}}).to_string()
                }
                _ => String::new(),
            })
        })
        .unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|(_, args)| args.as_str())
                .collect::<Vec<_>>(),
            [
                "simctl list runtimes -j",
                "simctl runtime list -j",
                "simctl shutdown all",
                "simctl delete all",
                "simctl runtime delete image"
            ]
        );
    }

    #[test]
    fn simulator_names_resolve_from_runtime_and_keep_only_selected_builds() {
        let spec = SimulatorConfig {
            version: "27.2".into(),
            devices: vec!["iPhone 17".into()],
        };
        let mut calls = Vec::new();
        install_simulators(&[spec], &mut |_, args| {
            calls.push(args.join(" "));
            Ok(match args {
                ["simctl", "list", "runtimes", "-j"] => runtime_catalog(),
                ["simctl", "runtime", "list", "-j"] => {
                    json!({"keep": {"build": "24C1"}, "remove": {"build": "old"}}).to_string()
                }
                _ => String::new(),
            })
        })
        .unwrap();
        assert!(calls.contains(&"simctl runtime delete remove".to_owned()));
        assert!(!calls.contains(&"simctl runtime delete keep".to_owned()));
        assert_eq!(
            calls.last().unwrap(),
            "simctl create iPhone 17 phone com.apple.CoreSimulator.SimRuntime.iOS-27-2"
        );
    }

    #[test]
    fn invalid_devices_and_failed_downloads_stop_before_cleanup() {
        for (version, name) in [("27.2", "unknown"), ("18.6", "iPhone 17")] {
            let spec = SimulatorConfig {
                version: version.into(),
                devices: vec![name.into()],
            };
            let mut downloaded = false;
            let error = install_simulators(&[spec], &mut |program, args| {
                if program == "xcodebuild" {
                    downloaded = true;
                    bail!("Runtime download failed");
                }
                assert_eq!(args, ["simctl", "list", "runtimes", "-j"]);
                Ok(runtime_catalog())
            })
            .unwrap_err();
            if version == "18.6" {
                assert!(downloaded);
                assert_eq!(error.to_string(), "Runtime download failed");
            } else {
                assert!(error.to_string().contains("does not support iOS"));
            }
        }
    }

    #[test]
    fn android_defaults_install_native_sdk_and_remove_inherited_emulators() {
        for extras in [
            vec![],
            vec![
                "emulator".to_owned(),
                "system-images;android-37;google_apis;arm64-v8a".to_owned(),
            ],
        ] {
            let temp = tempfile::tempdir().unwrap();
            let avds = temp.path().join(".android/avd");
            fs::create_dir_all(&avds).unwrap();
            fs::write(avds.join("old.avd"), "").unwrap();
            let golden = GoldenConfig {
                android_packages: extras.clone(),
                ..GoldenConfig::default()
            };
            let mut calls = Vec::new();
            let environment = install_android(&golden, temp.path().to_str().unwrap(), &mut |_, args| {
                calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
                Ok(if args.contains(&"--list") {
                    "platforms;android-37 | SDK\nbuild-tools;37.0.0 | tools\nndk;29.1.2 | NDK".into()
                } else if args.contains(&"--list_installed") {
                    "emulator | emulator\nsystem-images;android-37;google_apis;arm64-v8a | image\nsystem-images;android-34;google_apis;arm64-v8a | old image".into()
                } else { String::new() })
            }).unwrap();
            let install = calls
                .iter()
                .find(|args| args.iter().any(|arg| arg == "--install"))
                .unwrap();
            assert!(install.iter().any(|arg| arg == "platforms;android-37"));
            assert_eq!(
                install.iter().any(|arg| arg == "emulator"),
                !extras.is_empty()
            );
            let removed: Vec<_> = calls
                .iter()
                .filter(|args| args.iter().any(|arg| arg == "--uninstall"))
                .collect();
            assert_eq!(removed.len(), if extras.is_empty() { 3 } else { 1 });
            assert_eq!(avds.exists(), !extras.is_empty());
            assert!(environment.contains("/ndk/29.1.2"));
        }
    }

    #[test]
    fn stable_android_selection_uses_numeric_versions_and_excludes_previews() {
        let list = "platforms;android-9 | old\nplatforms;android-37 | stable\nplatforms;android-38-ext1 | extension\nplatforms;android-Preview | preview\nbuild-tools;37.0.0 | stable\nbuild-tools;38.0.0-rc1 | preview\nndk;29.1.2 | stable\nndk;9.9.9 | old";
        assert_eq!(
            latest_package(list, "platforms;android-").unwrap(),
            "platforms;android-37"
        );
        assert_eq!(
            latest_package(list, "build-tools;").unwrap(),
            "build-tools;37.0.0"
        );
        assert_eq!(latest_package(list, "ndk;").unwrap(), "ndk;29.1.2");
        assert!(latest_package("", "ndk;").is_err());
    }

    fn fake_app(app: &Path, version: &str) {
        fs::create_dir_all(app.join("Contents/Developer/usr/bin")).unwrap();
        fs::write(app.join("Contents/Developer/usr/bin/xcodebuild"), "").unwrap();
        fs::write(app.join("Contents/Info.plist"), version).unwrap();
    }

    #[test]
    fn xcodes_select_highest_version_and_reject_mismatched_archives() {
        for mismatch in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let apps = temp.path().join("Applications");
            fs::create_dir(&apps).unwrap();
            fake_app(&apps.join("Xcode.app"), "27.2");
            fs::write(temp.path().join("bakery-xcode-26.5.tar"), "").unwrap();
            let golden: GoldenConfig =
                serde_json::from_value(json!({"xcodes": ["27.2", "26.5"]})).unwrap();
            let mut calls = Vec::new();
            let result = install_xcodes(&golden, &apps, temp.path(), &mut |program, args| {
                calls.push(args.join(" "));
                if program == "plutil" {
                    return Ok(fs::read_to_string(args[5])?);
                }
                match args[0] {
                    "mv" => fs::rename(args[1], args[2])?,
                    "mkdir" => fs::create_dir(args[1])?,
                    "tar" => fake_app(Path::new(args[2]), if mismatch { "27.0" } else { "26.5" }),
                    _ => {}
                }
                Ok(String::new())
            });
            if mismatch {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("does not contain Xcode 26.5"));
                assert!(!calls.iter().any(|call| call.starts_with("xcode-select")));
            } else {
                result.unwrap();
                let selections: Vec<_> = calls
                    .iter()
                    .filter(|call| call.starts_with("xcode-select"))
                    .collect();
                assert_eq!(selections.len(), 2);
                assert!(selections[1].ends_with("Xcode-27.2.app/Contents/Developer"));
                assert!(!temp.path().join("bakery-xcode-26.5.tar").exists());
            }
        }
    }
}
