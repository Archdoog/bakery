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
    #[error(
        "Invalid root image for {image}: use a nonempty image name without control characters"
    )]
    InvalidRootImage { image: String },
    #[error("Invalid tools for golden {golden}: {reason}")]
    InvalidTools { golden: String, reason: String },
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
    for (image, source) in &cfg.root_images {
        if image.trim().is_empty()
            || image.chars().any(char::is_control)
            || source.trim().is_empty()
            || source.chars().any(char::is_control)
        {
            return Err(Error::InvalidRootImage {
                image: image.clone(),
            });
        }
    }
    for (golden, settings) in &cfg.goldens {
        let invalid = |reason: &str| Error::InvalidTools {
            golden: golden.clone(),
            reason: reason.into(),
        };
        if settings
            .source
            .as_deref()
            .is_some_and(|source| !valid_name(source))
        {
            return Err(invalid("Source must be a nonempty image name"));
        }
        let mut versions = HashSet::new();
        if settings
            .xcodes
            .iter()
            .any(|version| !numeric_version(version) || !versions.insert(version))
        {
            return Err(invalid(
                "Xcode overrides must be unique exact versions, such as 27.2",
            ));
        }
        let mut runtimes = HashSet::new();
        for runtime in &settings.simulators {
            if !numeric_version(&runtime.version) || !runtimes.insert(&runtime.version) {
                return Err(invalid("Simulator versions must be numeric and unique"));
            }
            let mut devices = HashSet::new();
            if runtime
                .devices
                .iter()
                .any(|name| !valid_name(name) || !devices.insert(name))
            {
                return Err(invalid(
                    "Simulator device names must be nonempty and unique within each runtime",
                ));
            }
        }
        if settings
            .android_packages
            .iter()
            .any(|package| !valid_name(package))
        {
            return Err(invalid("Android package names must be nonempty"));
        }
    }
    Ok(())
}

fn numeric_version(version: &str) -> bool {
    version.contains('.')
        && version.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_overrides_validate_before_a_bake() {
        for tools in [
            "{xcodes: ['27.2', '26.5'], simulators: [{version: '27.2', devices: ['iPhone 17 Pro Max']}]}",
            "{}",
        ] {
            let cfg: FleetConfig = serde_yaml_ng::from_str(&format!("github: {{org: test}}\nrunners: []\ngoldens: {{macos: {tools}}}")).unwrap();
            validate(&cfg).unwrap();
        }
        for tools in [
            "{xcodes: ['27']}",
            "{xcodes: ['27.2', '27.2']}",
            "{simulators: [{version: 'bad'}]}",
            "{simulators: [{version: '27.2', devices: ['Phone', 'Phone']}]}",
            "{android_packages: ['']}",
            "{source: ''}",
        ] {
            let cfg: FleetConfig = serde_yaml_ng::from_str(&format!(
                "github: {{org: test}}\nrunners: []\ngoldens: {{macos: {tools}}}"
            ))
            .unwrap();
            assert!(validate(&cfg).is_err(), "{tools}");
        }
    }

    #[test]
    fn removed_tool_fields_fail_instead_of_silently_using_defaults() {
        for tools in [
            "{default_xcode: '27.2'}",
            "{simulators: [{version: '27.2', donor_source: image}]}",
        ] {
            assert!(serde_yaml_ng::from_str::<FleetConfig>(&format!(
                "github: {{org: test}}\nrunners: []\ngoldens: {{macos: {tools}}}"
            ))
            .is_err());
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
