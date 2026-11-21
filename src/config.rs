//! Load and validate [`runners.yaml`] into the types defined in [`crate::models`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::models::FleetConfig;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config not found at {0} — copy runners.yaml.example to runners.yaml")]
    NotFound(PathBuf),
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parsing {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("duplicate runner group name: {0}")]
    DuplicateGroup(String),
    #[error("invalid simulator configuration for golden {golden}: {reason}")]
    InvalidSimulators { golden: String, reason: String },
    #[error("invalid Xcode configuration for golden {golden}: {reason}")]
    InvalidXcode { golden: String, reason: String },
}

pub fn load(path: &Path) -> Result<FleetConfig, Error> {
    if !path.exists() {
        return Err(Error::NotFound(path.to_path_buf()));
    }
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: FleetConfig = serde_yaml_ng::from_str(&text).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    validate(&cfg)?;
    Ok(cfg)
}

fn validate(cfg: &FleetConfig) -> Result<(), Error> {
    let mut seen: HashSet<&str> = HashSet::new();
    for g in &cfg.runners {
        if !seen.insert(&g.name) {
            return Err(Error::DuplicateGroup(g.name.clone()));
        }
    }
    for (golden, settings) in &cfg.goldens {
        let invalid_xcode = |reason: &str| Error::InvalidXcode {
            golden: golden.clone(),
            reason: reason.into(),
        };
        let mut versions = HashSet::new();
        for xcode in &settings.xcodes {
            if !numeric_version(&xcode.version) || !versions.insert(&xcode.version) {
                return Err(invalid_xcode("Xcode versions must be numeric and unique"));
            }
            if xcode.source.as_deref().is_some_and(|source| {
                source.trim().is_empty() || source.chars().any(char::is_control)
            }) {
                return Err(invalid_xcode(
                    "Xcode source must contain a valid Tart image name",
                ));
            }
        }
        for left in &settings.xcodes {
            for right in &settings.xcodes {
                if left.version != right.version
                    && !right.version.contains('.')
                    && left
                        .version
                        .strip_prefix(&right.version)
                        .is_some_and(|suffix| suffix.starts_with('.'))
                {
                    return Err(invalid_xcode("Xcode version ranges must not overlap"));
                }
            }
        }
        if settings
            .source
            .as_deref()
            .is_some_and(|source| source.trim().is_empty() || source.chars().any(char::is_control))
        {
            return Err(invalid_xcode(
                "Golden source must contain a valid Tart image name",
            ));
        }
        if settings
            .default_xcode
            .as_ref()
            .is_some_and(|version| !versions.contains(version))
        {
            return Err(invalid_xcode(
                "default_xcode must name a configured Xcode version",
            ));
        }
        let mut runtimes = HashSet::new();
        for runtime in &settings.simulators {
            let invalid = |reason: &str| Error::InvalidSimulators {
                golden: golden.clone(),
                reason: reason.to_string(),
            };
            if !numeric_version(&runtime.version) {
                return Err(invalid("use a quoted numeric version, such as \"27.0\""));
            }
            if runtime
                .xcode
                .as_ref()
                .is_some_and(|version| !versions.contains(version))
            {
                return Err(invalid_xcode(
                    "Simulator xcode must name a configured Xcode version",
                ));
            }
            if !runtimes.insert((runtime.platform, &runtime.version)) {
                return Err(invalid("each platform version must occur only once"));
            }
            for value in [&runtime.runtime_image, &runtime.donor_source]
                .into_iter()
                .flatten()
            {
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(invalid(
                        "runtime_image and donor_source must contain a valid path or image name",
                    ));
                }
            }
            let mut names = HashSet::new();
            for device in &runtime.devices {
                if device.name.trim().is_empty()
                    || device.name.chars().any(char::is_control)
                    || !device
                        .device_type
                        .starts_with("com.apple.CoreSimulator.SimDeviceType.")
                    || device.device_type.chars().any(char::is_control)
                {
                    return Err(invalid(
                        "each device needs a name and a CoreSimulator device type identifier",
                    ));
                }
                if !names.insert(&device.name) {
                    return Err(invalid("device names must be unique within each runtime"));
                }
            }
        }
    }
    Ok(())
}

fn numeric_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(selection: &str) -> FleetConfig {
        serde_yaml_ng::from_str(&format!(
            "github: {{org: test}}\nrunners: []\ngoldens:\n  macos:\n    simulators:\n{selection}"
        ))
        .unwrap()
    }

    #[test]
    fn example_and_legacy_configuration_are_valid() {
        let example: FleetConfig =
            serde_yaml_ng::from_str(include_str!("../runners.yaml.example")).unwrap();
        validate(&example).unwrap();
        assert_eq!(example.goldens["macos"].simulators.len(), 2);
        let legacy: FleetConfig =
            serde_yaml_ng::from_str("github: {org: test}\nrunners: []").unwrap();
        validate(&legacy).unwrap();
        assert!(legacy.goldens.is_empty());
    }

    #[test]
    fn accepts_runtime_without_devices_and_empty_selection() {
        let cfg = parse("      - {platform: iOS, version: \"27.0\"}");
        validate(&cfg).unwrap();
        assert!(cfg.goldens["macos"].simulators[0].devices.is_empty());
        let cfg: FleetConfig = serde_yaml_ng::from_str(
            "github: {org: test}\nrunners: []\ngoldens: {macos: {simulators: []}}",
        )
        .unwrap();
        validate(&cfg).unwrap();
    }

    #[test]
    fn rejects_duplicate_runtimes_and_invalid_versions() {
        for selection in [
            "      - {platform: iOS, version: \"27.0\"}\n      - {platform: iOS, version: \"27.0\"}",
            "      - {platform: iOS, version: \"27.0; echo bad\"}",
            "      - {platform: iOS, version: \"\"}",
        ] {
            assert!(validate(&parse(selection)).is_err());
        }
    }

    #[test]
    fn rejects_invalid_and_duplicate_devices() {
        for devices in [
            "[{name: '', device_type: invalid}]",
            "[{name: Phone, device_type: invalid}]",
            "[{name: Phone, device_type: com.apple.CoreSimulator.SimDeviceType.iPhone-16}, {name: Phone, device_type: com.apple.CoreSimulator.SimDeviceType.iPhone-16}]",
        ] {
            let cfg = parse(&format!("      - {{platform: iOS, version: \"18.6\", devices: {devices}}}"));
            assert!(validate(&cfg).is_err());
        }
    }

    #[test]
    fn rejects_unknown_simulator_fields_and_platforms() {
        for selection in [
            "{platform: Android, version: '34'}",
            "{platform: iOS, version: '27.0', device: []}",
        ] {
            assert!(serde_yaml_ng::from_str::<FleetConfig>(&format!(
                "github: {{org: test}}\nrunners: []\ngoldens: {{macos: {{simulators: [{selection}]}}}}"
            )).is_err());
        }
    }
    #[test]
    fn rejects_unknown_defaults_runtime_toolchains_and_overlapping_xcodes() {
        for golden in [
            "{xcodes: [{version: '27'}], default_xcode: '26.5'}",
            "{xcodes: [{version: '27'}, {version: '27.0'}]}",
            "{xcodes: [{version: '27'}, {version: '27'}]}",
            "{xcodes: [{version: '../27'}]}",
            "{xcodes: [{version: '27', source: ''}]}",
            "{simulators: [{platform: iOS, version: '18.6', xcode: '26.5'}]}",
        ] {
            let cfg: FleetConfig = serde_yaml_ng::from_str(&format!(
                "github: {{org: test}}\nrunners: []\ngoldens: {{macos: {golden}}}"
            ))
            .unwrap();
            assert!(validate(&cfg).is_err(), "{golden}");
        }
    }

    #[test]
    fn yaml_example_enables_ephemeral_and_omission_defaults_to_persistent() {
        let yaml = include_str!("../runners.yaml.example");
        let cfg: FleetConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(cfg.runners.iter().all(|g| g.ephemeral));
        assert_eq!(cfg.runner_version, "latest");
        let pinned: FleetConfig =
            serde_yaml_ng::from_str(&format!("{yaml}\nrunner_version: 2.337.0\n")).unwrap();
        assert_eq!(pinned.runner_version, "2.337.0");
        let cfg: FleetConfig =
            serde_yaml_ng::from_str(&yaml.replace("ephemeral: true", "ephemeral: false")).unwrap();
        assert!(cfg.runners.iter().all(|g| !g.ephemeral));
        let cfg: FleetConfig =
            serde_yaml_ng::from_str(&yaml.replace("    ephemeral: true\n", "")).unwrap();
        assert!(cfg.runners.iter().all(|g| !g.ephemeral));
    }
}
