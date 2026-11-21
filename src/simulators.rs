//! Configure simulator runtimes and devices during a macOS bake.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use clap::Subcommand;
use serde::Deserialize;
use thiserror::Error;

use crate::models::fleet::{SimulatorConfig, SimulatorPlatform};

/// Reports simulator data, filesystem, and command errors during a bake.
#[derive(Debug, Error)]
pub enum SimulatorError {
    #[error("Simulator filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Invalid simulator JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid UTF-8 command output: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("Cannot start {program}: {source}")]
    Spawn { program: String, source: io::Error },
    #[error("{program} failed with {status}")]
    Command {
        program: String,
        status: std::process::ExitStatus,
    },
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, SimulatorError>;

/// Defines internal simulator operations for the bake scripts.
#[derive(Debug, Subcommand)]
pub enum Action {
    /// Write manifest fields as records separated by null bytes.
    Fields { manifest: PathBuf },
    /// Check stdin for an available runtime with the given platform and version.
    Contains { platform: String, version: String },
    /// Find a host runtime image in stdin for the given platform and version.
    FindImage { platform: String, version: String },
    /// Install the manifest's runtimes and devices in the guest.
    Install { manifest: PathBuf },
}

#[derive(Debug, Deserialize)]
struct RuntimeList {
    runtimes: Vec<Runtime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    version: String,
    platform: Option<String>,
    buildversion: String,
    #[serde(default)]
    is_available: bool,
    #[serde(default)]
    supported_device_types: Vec<DeviceType>,
}

#[derive(Debug, Deserialize)]
struct DeviceType {
    identifier: String,
}

#[derive(Debug, Deserialize)]
struct DeviceList {
    devices: BTreeMap<String, Vec<Device>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    name: String,
    udid: String,
    device_type_identifier: Option<String>,
    #[serde(default)]
    is_available: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Image {
    runtime_identifier: Option<String>,
    build: Option<String>,
    version: Option<String>,
    state: Option<String>,
    path: Option<PathBuf>,
    platform_identifier: Option<String>,
}

fn platform_name(spec: &SimulatorConfig) -> &'static str {
    match spec.platform {
        SimulatorPlatform::Ios => "iOS",
        SimulatorPlatform::Watchos => "watchOS",
        SimulatorPlatform::Tvos => "tvOS",
        SimulatorPlatform::Visionos => "visionOS",
    }
}

fn matches(runtime: &Runtime, platform: &str, version: &str) -> bool {
    runtime.is_available
        && runtime.platform.as_deref() == Some(platform)
        && runtime.version == version
}

/// Execute an internal bake `action` and return its exit code.
///
/// # Errors
/// Return an error if simulator data is invalid or a filesystem operation or command fails.
pub fn run(action: Action) -> Result<u8> {
    match action {
        Action::Fields { manifest } => {
            let specs: Vec<SimulatorConfig> = serde_json::from_slice(&fs::read(manifest)?)?;
            let mut stdout = io::stdout().lock();
            for (index, spec) in specs.iter().enumerate() {
                for field in [
                    index.to_string(),
                    platform_name(spec).to_owned(),
                    spec.version.clone(),
                    spec.runtime_image.clone().unwrap_or_default(),
                    spec.donor_source.clone().unwrap_or_default(),
                    spec.xcode.clone().unwrap_or_default(),
                ] {
                    stdout.write_all(field.as_bytes())?;
                    stdout.write_all(&[0])?;
                }
            }
        }
        Action::Contains { platform, version } => {
            let list: RuntimeList = serde_json::from_reader(io::stdin().lock())?;
            return Ok(u8::from(
                !list
                    .runtimes
                    .iter()
                    .any(|r| matches(r, &platform, &version)),
            ));
        }
        Action::FindImage { platform, version } => {
            let images: BTreeMap<String, Image> = serde_json::from_reader(io::stdin().lock())?;
            if let Some(path) = find_image(&images, &platform, &version) {
                println!("{}", path.display());
            }
        }
        Action::Install { manifest } => {
            let specs: Vec<SimulatorConfig> = serde_json::from_slice(&fs::read(manifest)?)?;
            install(&specs, Path::new("/tmp"), &mut command)?;
        }
    }
    Ok(0)
}

fn find_image<'a>(
    images: &'a BTreeMap<String, Image>,
    platform: &str,
    version: &str,
) -> Option<&'a Path> {
    let platform_id = match platform {
        "iOS" => "com.apple.platform.iphonesimulator",
        "watchOS" => "com.apple.platform.watchsimulator",
        "tvOS" => "com.apple.platform.appletvsimulator",
        "visionOS" => "com.apple.platform.xrsimulator",
        _ => return None,
    };
    images.values().find_map(|image| {
        let path = image.path.as_deref()?;
        (image.version.as_deref() == Some(version)
            && image.state.as_deref() == Some("Ready")
            && image.platform_identifier.as_deref() == Some(platform_id)
            && path.is_file())
        .then_some(path)
    })
}

fn command(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|source| SimulatorError::Spawn {
            program: program.into(),
            source,
        })?;
    if !output.status.success() {
        return Err(SimulatorError::Command {
            program: program.into(),
            status: output.status,
        });
    }
    Ok(String::from_utf8(output.stdout)?)
}

type RunCommand<'a> = dyn FnMut(&str, &[&str]) -> Result<String> + 'a;

fn simctl(run: &mut RunCommand<'_>, args: &[&str]) -> Result<String> {
    run("xcrun", &[&["simctl"], args].concat())
}

fn runtimes(run: &mut RunCommand<'_>) -> Result<Vec<Runtime>> {
    Ok(serde_json::from_str::<RuntimeList>(&simctl(run, &["list", "runtimes", "-j"])?)?.runtimes)
}

fn wait_services(run: &mut RunCommand<'_>) -> Result<()> {
    for attempt in 0..60 {
        if runtimes(run).is_ok() && simctl(run, &["runtime", "list", "-j"]).is_ok() {
            return Ok(());
        }
        if attempt < 59 {
            sleep(Duration::from_secs(5));
        }
    }
    Err(SimulatorError::Invalid(
        "Simulator services did not respond after 5 minutes".into(),
    ))
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| SimulatorError::Invalid(format!("Invalid UTF-8 path: {}", path.display())))
}

fn import_image(run: &mut RunCommand<'_>, image: &Path) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mount = directory.path().join("mount");
    fs::create_dir(&mount)?;
    let attached = run(
        "hdiutil",
        &[
            "attach",
            "-readonly",
            "-nobrowse",
            "-noverify",
            "-mountpoint",
            path_text(&mount)?,
            path_text(image)?,
        ],
    )
    .is_ok();
    let source = if attached {
        let copied = (|| -> Result<Option<PathBuf>> {
            let restore = mount.join("Restore");
            if !restore.is_dir() {
                return Ok(None);
            }
            let mut images = fs::read_dir(restore)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<io::Result<Vec<_>>>()?;
            images.retain(|path| path.extension().is_some_and(|extension| extension == "dmg"));
            images.sort();
            if let Some(inner) = images.first() {
                // simctl cannot import an image from a read-only bundle volume.
                let dest = directory.path().join("runtime.dmg");
                fs::copy(inner, &dest)?;
                return Ok(Some(dest));
            }
            Ok(None)
        })();
        run("hdiutil", &["detach", path_text(&mount)?, "-quiet"])?;
        copied?.unwrap_or_else(|| image.to_path_buf())
    } else {
        image.to_path_buf()
    };
    simctl(run, &["runtime", "add", path_text(&source)?])?;
    Ok(())
}

fn scoped_command(
    run: &mut RunCommand<'_>,
    xcode: Option<&str>,
    program: &str,
    args: &[&str],
) -> Result<String> {
    if let Some(version) = xcode {
        let developer =
            format!("DEVELOPER_DIR=/Applications/Xcode-{version}.app/Contents/Developer");
        run("env", &[&[developer.as_str(), program], args].concat())
    } else {
        run(program, args)
    }
}

fn install(specs: &[SimulatorConfig], image_dir: &Path, run: &mut RunCommand<'_>) -> Result<()> {
    wait_services(run)?;
    for (index, spec) in specs.iter().enumerate() {
        let mut scoped = |program: &str, args: &[&str]| {
            scoped_command(run, spec.xcode.as_deref(), program, args)
        };
        let platform = platform_name(spec);
        let image = image_dir.join(format!("bakery-runtime-{index}.dmg"));
        if !runtimes(&mut scoped)?
            .iter()
            .any(|runtime| matches(runtime, platform, &spec.version))
        {
            if image.is_file() {
                import_image(&mut scoped, &image)?;
            } else {
                scoped(
                    "xcodebuild",
                    &[
                        "-downloadPlatform",
                        platform,
                        "-buildVersion",
                        &spec.version,
                        "-architectureVariant",
                        "arm64",
                    ],
                )?;
            }
            let mut found = false;
            for _ in 0..15 {
                if runtimes(&mut scoped)?
                    .iter()
                    .any(|runtime| matches(runtime, platform, &spec.version))
                {
                    found = true;
                    break;
                }
                sleep(Duration::from_secs(2));
            }
            if !found {
                return Err(SimulatorError::Invalid(format!(
                    "No available {platform} {} runtime after installation",
                    spec.version
                )));
            }
        }
        if image.exists() {
            fs::remove_file(image)?;
        }
    }
    let mut available = runtimes(run)?;
    for spec in specs.iter().filter(|spec| spec.xcode.is_some()) {
        let mut scoped = |program: &str, args: &[&str]| {
            scoped_command(run, spec.xcode.as_deref(), program, args)
        };
        for runtime in runtimes(&mut scoped)? {
            if let Some(index) = available.iter().position(|existing| {
                existing.identifier == runtime.identifier
                    && existing.buildversion == runtime.buildversion
            }) {
                if runtime.is_available {
                    available[index] = runtime;
                }
            } else {
                available.push(runtime);
            }
        }
    }
    let devices: DeviceList = serde_json::from_str(&simctl(run, &["list", "devices", "-j"])?)?;
    let images: BTreeMap<String, Image> =
        serde_json::from_str(&simctl(run, &["runtime", "list", "-j"])?)?;
    for action in reconcile(specs, &available, &devices, &images)? {
        let xcode = if action.first().is_some_and(|name| name == "create") {
            specs
                .iter()
                .find(|spec| {
                    spec.devices
                        .iter()
                        .any(|device| device.name == action[1] && device.device_type == action[2])
                        && available.iter().any(|runtime| {
                            runtime.identifier == action[3]
                                && matches(runtime, platform_name(spec), &spec.version)
                        })
                })
                .and_then(|spec| spec.xcode.as_deref())
        } else {
            None
        };
        let mut scoped = |program: &str, args: &[&str]| scoped_command(run, xcode, program, args);
        simctl(
            &mut scoped,
            &action.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
    }
    Ok(())
}

fn reconcile(
    specs: &[SimulatorConfig],
    runtimes: &[Runtime],
    devices: &DeviceList,
    images: &BTreeMap<String, Image>,
) -> Result<Vec<Vec<String>>> {
    let selected: Vec<_> = runtimes
        .iter()
        .filter(|runtime| {
            specs
                .iter()
                .any(|spec| matches(runtime, platform_name(spec), &spec.version))
        })
        .collect();
    for spec in specs {
        if !selected
            .iter()
            .any(|runtime| matches(runtime, platform_name(spec), &spec.version))
        {
            return Err(SimulatorError::Invalid(format!(
                "No available {} {} runtime after installation",
                platform_name(spec),
                spec.version
            )));
        }
    }
    let mut desired = BTreeSet::new();
    for spec in specs {
        let platform = platform_name(spec);
        for device in &spec.devices {
            let runtime = selected
                .iter()
                .find(|runtime| {
                    matches(runtime, platform, &spec.version)
                        && runtime
                            .supported_device_types
                            .iter()
                            .any(|kind| kind.identifier == device.device_type)
                })
                .ok_or_else(|| {
                    SimulatorError::Invalid(format!(
                        "Device type {} does not support {platform} {}",
                        device.device_type, spec.version
                    ))
                })?;
            desired.insert((
                runtime.identifier.clone(),
                device.name.clone(),
                device.device_type.clone(),
            ));
        }
    }
    let mut actions = Vec::new();
    let mut retained = BTreeSet::new();
    for (runtime, devices) in &devices.devices {
        for device in devices {
            let key = (
                runtime.clone(),
                device.name.clone(),
                device.device_type_identifier.clone().unwrap_or_default(),
            );
            if device.is_available && desired.contains(&key) && retained.insert(key) {
                continue;
            }
            actions.push(vec!["delete".into(), device.udid.clone()]);
        }
    }
    for (identifier, image) in images {
        if !selected.iter().any(|runtime| {
            image.runtime_identifier.as_deref() == Some(runtime.identifier.as_str())
                && image.build.as_deref() == Some(runtime.buildversion.as_str())
        }) {
            actions.push(vec!["runtime".into(), "delete".into(), identifier.clone()]);
        }
    }
    for (runtime, name, kind) in desired.difference(&retained) {
        actions.push(vec![
            "create".into(),
            name.clone(),
            kind.clone(),
            runtime.clone(),
        ]);
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const KIND: &str = "com.apple.CoreSimulator.SimDeviceType.iPhone-16";

    fn spec() -> SimulatorConfig {
        serde_json::from_value(json!({
            "platform": "iOS", "version": "18.6",
            "devices": [{"name": "CI Phone", "device_type": KIND}]
        }))
        .unwrap()
    }

    fn runtime() -> Value {
        json!({
            "identifier": "ios-18-6", "version": "18.6", "platform": "iOS",
            "isAvailable": true, "buildversion": "22G86",
            "supportedDeviceTypes": [{"identifier": KIND}]
        })
    }

    fn actions(
        specs: &[SimulatorConfig],
        devices: Value,
        images: Value,
    ) -> Result<Vec<Vec<String>>> {
        reconcile(
            specs,
            &[serde_json::from_value(runtime())?],
            &serde_json::from_value(json!({"devices": devices}))?,
            &serde_json::from_value(images)?,
        )
    }

    #[test]
    fn retains_selected_device_and_deletes_duplicates_defaults_and_old_images() {
        let actions = actions(&[spec()], json!({"ios-18-6": [
            {"name": "CI Phone", "deviceTypeIdentifier": KIND, "udid": "keep", "isAvailable": true},
            {"name": "CI Phone", "deviceTypeIdentifier": KIND, "udid": "duplicate", "isAvailable": true},
            {"name": "Default", "udid": "default"}
        ]}), json!({
            "keep": {"runtimeIdentifier": "ios-18-6", "build": "22G86"},
            "old": {"runtimeIdentifier": "ios-18-5", "build": "old"}
        })).unwrap();
        assert_eq!(
            actions,
            vec![
                vec!["delete", "duplicate"],
                vec!["delete", "default"],
                vec!["runtime", "delete", "old"]
            ]
        );
    }

    #[test]
    fn creates_named_devices_with_literal_shell_characters() {
        let mut spec = spec();
        spec.devices[0].name = "CI ' $(touch bad)".into();
        assert_eq!(
            actions(&[spec], json!({}), json!({})).unwrap(),
            vec![vec!["create", "CI ' $(touch bad)", KIND, "ios-18-6"]]
        );
    }

    #[test]
    fn empty_selection_removes_inherited_devices_and_images() {
        let actions = actions(
            &[],
            json!({"ios-18-6": [{"name": "Default", "udid": "default"}]}),
            json!({"image": {"runtimeIdentifier": "ios-18-6", "build": "22G86"}}),
        )
        .unwrap();
        assert_eq!(
            actions,
            vec![
                vec!["delete", "default"],
                vec!["runtime", "delete", "image"]
            ]
        );
    }

    #[test]
    fn runtime_only_selection_creates_no_devices() {
        let mut spec = spec();
        spec.devices.clear();
        assert!(actions(&[spec], json!({}), json!({})).unwrap().is_empty());
    }

    #[test]
    fn runtime_match_requires_exact_platform_version_and_availability() {
        let runtime: Runtime = serde_json::from_value(runtime()).unwrap();
        assert!(matches(&runtime, "iOS", "18.6"));
        assert!(!matches(&runtime, "tvOS", "18.6"));
        assert!(!matches(&runtime, "iOS", "18.6.1"));
        assert!(!matches(
            &Runtime {
                is_available: false,
                ..runtime
            },
            "iOS",
            "18.6"
        ));
    }

    #[test]
    fn missing_runtime_returns_no_removal_plan() {
        let devices = DeviceList {
            devices: BTreeMap::new(),
        };
        assert!(reconcile(&[spec()], &[], &devices, &BTreeMap::new()).is_err());
    }

    #[test]
    fn incompatible_device_returns_no_removal_plan() {
        let mut spec = spec();
        spec.devices[0].device_type = "unsupported".into();
        assert!(actions(&[spec], json!({}), json!({}))
            .unwrap_err()
            .to_string()
            .contains("does not support"));
    }

    #[test]
    fn download_failure_stops_before_device_removal() {
        let dir = tempfile::tempdir().unwrap();
        let mut calls = Vec::new();
        let result = install(&[spec()], dir.path(), &mut |program, args| {
            calls.push((program.to_owned(), args.join(" ")));
            if program == "xcodebuild" {
                return Err(SimulatorError::Invalid("Download failed".into()));
            }
            Ok(if args == ["simctl", "list", "runtimes", "-j"] {
                json!({"runtimes": []}).to_string()
            } else {
                "{}".into()
            })
        });
        assert_eq!(result.unwrap_err().to_string(), "Download failed");
        assert!(calls.iter().any(|(program, _)| program == "xcodebuild"));
        assert!(!calls.iter().any(|(_, args)| args.contains("delete")));
    }

    #[test]
    fn downloads_arm64_runtime_before_creating_device() {
        let dir = tempfile::tempdir().unwrap();
        let mut downloaded = false;
        let mut calls = Vec::new();
        install(&[spec()], dir.path(), &mut |program, args| {
            calls.push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            ));
            if program == "xcodebuild" {
                downloaded = true;
            }
            Ok(if args == ["simctl", "list", "runtimes", "-j"] {
                json!({"runtimes": if downloaded { vec![runtime()] } else { vec![] }}).to_string()
            } else if args == ["simctl", "list", "devices", "-j"] {
                json!({"devices": {}}).to_string()
            } else {
                "{}".into()
            })
        })
        .unwrap();
        assert!(calls.iter().any(|(program, args)| program == "xcodebuild"
            && args
                == &[
                    "-downloadPlatform",
                    "iOS",
                    "-buildVersion",
                    "18.6",
                    "-architectureVariant",
                    "arm64"
                ]));
        assert!(calls
            .iter()
            .any(|(_, args)| args == &["simctl", "create", "CI Phone", KIND, "ios-18-6"]));
    }

    #[test]
    fn imports_raw_image_without_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("bakery-runtime-0.dmg");
        fs::write(&image, "image").unwrap();
        let mut imported = false;
        install(&[spec()], dir.path(), &mut |program, args| {
            assert_ne!(program, "xcodebuild");
            if program == "hdiutil" {
                return Err(SimulatorError::Invalid("Raw image".into()));
            }
            if args.starts_with(&["simctl", "runtime", "add"]) {
                assert_eq!(Path::new(args[3]), image);
                imported = true;
            }
            Ok(if args == ["simctl", "list", "runtimes", "-j"] {
                json!({"runtimes": if imported { vec![runtime()] } else { vec![] }}).to_string()
            } else if args == ["simctl", "list", "devices", "-j"] {
                json!({"devices": {}}).to_string()
            } else {
                "{}".into()
            })
        })
        .unwrap();
        assert!(imported);
        assert!(!image.exists());
    }

    #[test]
    fn bundle_import_copies_inner_image_and_detaches_before_import() {
        let dir = tempfile::tempdir().unwrap();
        let mut detached = false;
        let mut imported = false;
        import_image(
            &mut |program, args| {
                if program == "hdiutil" && args[0] == "attach" {
                    let restore = Path::new(args[5]).join("Restore");
                    fs::create_dir(&restore)?;
                    fs::write(restore.join("inner.dmg"), "inner image")?;
                } else if program == "hdiutil" && args[0] == "detach" {
                    detached = true;
                } else if program == "xcrun" {
                    assert!(detached);
                    assert_eq!(fs::read_to_string(args[3])?, "inner image");
                    imported = true;
                }
                Ok(String::new())
            },
            &dir.path().join("bundle.dmg"),
        )
        .unwrap();
        assert!(imported);
    }

    #[test]
    fn bundle_copy_failure_still_detaches_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let mut detached = false;
        let result = import_image(
            &mut |program, args| {
                if program == "hdiutil" && args[0] == "attach" {
                    let restore = Path::new(args[5]).join("Restore");
                    fs::create_dir(&restore)?;
                    fs::create_dir(restore.join("not-a-file.dmg"))?;
                } else if program == "hdiutil" && args[0] == "detach" {
                    detached = true;
                } else {
                    panic!("Cannot import an image after a copy failure");
                }
                Ok(String::new())
            },
            &dir.path().join("bundle.dmg"),
        );
        assert!(result.is_err());
        assert!(detached);
    }

    #[test]
    fn host_image_selection_ignores_other_platforms_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("runtime image.dmg");
        fs::write(&file, "image").unwrap();
        let images = serde_json::from_value(json!({
            "wrong-platform": {"version": "18.6", "state": "Ready", "path": file,
                               "platformIdentifier": "com.apple.platform.appletvsimulator"},
            "directory": {"version": "18.6", "state": "Ready", "path": dir.path(),
                          "platformIdentifier": "com.apple.platform.iphonesimulator"},
            "match": {"version": "18.6", "state": "Ready", "path": file,
                      "platformIdentifier": "com.apple.platform.iphonesimulator"}
        }))
        .unwrap();
        assert_eq!(find_image(&images, "iOS", "18.6"), Some(file.as_path()));
        assert!(find_image(&images, "iOS", "18.5").is_none());
        let wrong_platform = serde_json::from_value(json!({
            "image": {"version": "18.6", "state": "Ready", "path": file,
                      "platformIdentifier": "com.apple.platform.appletvsimulator"}
        }))
        .unwrap();
        assert!(find_image(&wrong_platform, "iOS", "18.6").is_none());
    }
    #[test]
    fn selected_xcode_downloads_and_creates_devices_when_default_cannot_use_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = spec();
        spec.xcode = Some("26.5".into());
        let mut downloaded = false;
        let mut created = false;
        install(&[spec], dir.path(), &mut |program, args| {
            let scoped = program == "env";
            let (program, args) = if scoped {
                assert_eq!(
                    args[0],
                    "DEVELOPER_DIR=/Applications/Xcode-26.5.app/Contents/Developer"
                );
                (args[1], &args[2..])
            } else {
                (program, args)
            };
            if program == "xcodebuild" {
                assert!(scoped);
                downloaded = true;
            }
            if args.starts_with(&["simctl", "create"]) {
                assert!(scoped);
                created = true;
            }
            Ok(if args == ["simctl", "list", "runtimes", "-j"] {
                let mut entry = runtime();
                entry["isAvailable"] = json!(scoped && downloaded);
                json!({"runtimes": [entry]}).to_string()
            } else if args == ["simctl", "list", "devices", "-j"] {
                json!({"devices": {}}).to_string()
            } else {
                "{}".into()
            })
        })
        .unwrap();
        assert!(downloaded && created);
    }
}
