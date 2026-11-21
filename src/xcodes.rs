//! Install and select Xcode versions during a macOS bake.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Subcommand;
use serde::Deserialize;
use thiserror::Error;

use crate::models::fleet::GoldenConfig;

/// Reports configuration, filesystem, and command errors during Xcode installation.
#[derive(Debug, Error)]
pub enum XcodeError {
    #[error("Xcode filesystem operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Invalid Xcode JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, XcodeError>;

/// Defines internal Xcode operations for the bake scripts.
#[derive(Debug, Subcommand)]
pub enum Action {
    /// Write Xcode versions and source images from a golden manifest.
    Fields { manifest: PathBuf },
    /// Check whether the requested Xcode version exists in the guest.
    Contains { version: String },
    /// Export the requested Xcode version to an archive in the donor guest.
    Export { version: String, archive: PathBuf },
    /// Install available archives and select the manifest's default Xcode.
    Install { manifest: PathBuf },
}

fn command(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(XcodeError::Invalid(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| XcodeError::Invalid(format!("Invalid UTF-8 path: {}", path.display())))
}

fn version_matches(actual: &str, requested: &str) -> bool {
    actual == requested
        || !requested.contains('.')
            && actual
                .strip_prefix(requested)
                .is_some_and(|suffix| suffix.starts_with('.'))
}

#[derive(Deserialize)]
struct VersionInfo {
    #[serde(rename = "CFBundleShortVersionString")]
    version: String,
}

type RunCommand<'a> = dyn FnMut(&str, &[&str]) -> Result<String> + 'a;

fn app_version(run: &mut RunCommand<'_>, app: &Path) -> Result<String> {
    let plist = app.join("Contents/Info.plist");
    let json = run(
        "plutil",
        &["-convert", "json", "-o", "-", path_text(&plist)?],
    )?;
    Ok(serde_json::from_str::<VersionInfo>(&json)?.version)
}

fn find_app(
    run: &mut RunCommand<'_>,
    applications: &Path,
    version: &str,
) -> Result<Option<PathBuf>> {
    let mut apps = fs::read_dir(applications)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    apps.sort();
    for app in apps {
        if app.join("Contents/Developer/usr/bin/xcodebuild").is_file()
            && version_matches(&app_version(run, &app)?, version)
        {
            return Ok(Some(app));
        }
    }
    Ok(None)
}

fn canonical_app(applications: &Path, version: &str) -> PathBuf {
    applications.join(format!("Xcode-{version}.app"))
}

/// Execute an Xcode bake `action` and return its exit code.
///
/// # Errors
/// Return an error if configuration is invalid or a filesystem operation or command fails.
pub fn run(action: Action) -> Result<u8> {
    let applications = Path::new("/Applications");
    match action {
        Action::Fields { manifest } => {
            let golden: GoldenConfig = serde_json::from_slice(&fs::read(manifest)?)?;
            let mut stdout = io::stdout().lock();
            for xcode in golden.xcodes {
                for field in [xcode.version.clone(), xcode.source_image()] {
                    stdout.write_all(field.as_bytes())?;
                    stdout.write_all(&[0])?;
                }
            }
        }
        Action::Contains { version } => {
            return Ok(u8::from(
                find_app(&mut command, applications, &version)?.is_none(),
            ));
        }
        Action::Export { version, archive } => {
            let app = find_app(&mut command, applications, &version)?.ok_or_else(|| {
                XcodeError::Invalid(format!(
                    "Xcode {version} is not installed in the donor image"
                ))
            })?;
            command(
                "tar",
                &["-C", path_text(&app)?, "-cf", path_text(&archive)?, "."],
            )?;
        }
        Action::Install { manifest } => {
            let golden: GoldenConfig = serde_json::from_slice(&fs::read(manifest)?)?;
            install(&golden, applications, Path::new("/tmp"), &mut command)?;
        }
    }
    Ok(0)
}

fn install(
    golden: &GoldenConfig,
    applications: &Path,
    image_dir: &Path,
    run: &mut RunCommand<'_>,
) -> Result<()> {
    for xcode in &golden.xcodes {
        let dest = canonical_app(applications, &xcode.version);
        match find_app(run, applications, &xcode.version)? {
            Some(app) if app != dest => {
                if dest.exists() {
                    return Err(XcodeError::Invalid(format!(
                        "Xcode destination already exists: {}",
                        dest.display()
                    )));
                }
                run("sudo", &["mv", path_text(&app)?, path_text(&dest)?])?;
            }
            Some(_) => {}
            None => {
                if dest.exists() {
                    return Err(XcodeError::Invalid(format!(
                        "Xcode destination already exists: {}",
                        dest.display()
                    )));
                }
                let archive = image_dir.join(format!("bakery-xcode-{}.tar", xcode.version));
                run("sudo", &["mkdir", path_text(&dest)?])?;
                run(
                    "sudo",
                    &["tar", "-C", path_text(&dest)?, "-xf", path_text(&archive)?],
                )?;
                if !version_matches(&app_version(run, &dest)?, &xcode.version) {
                    return Err(XcodeError::Invalid(format!(
                        "Archive does not contain Xcode {}",
                        xcode.version
                    )));
                }
                fs::remove_file(archive)?;
            }
        }
    }
    let default = golden
        .default_xcode
        .as_deref()
        .or_else(|| golden.xcodes.first().map(|xcode| xcode.version.as_str()));
    let ordered = golden
        .xcodes
        .iter()
        .filter(|xcode| Some(xcode.version.as_str()) != default)
        .chain(
            golden
                .xcodes
                .iter()
                .filter(|xcode| Some(xcode.version.as_str()) == default),
        );
    for xcode in ordered {
        let dest = canonical_app(applications, &xcode.version);
        let build = dest.join("Contents/Developer/usr/bin/xcodebuild");
        let developer = format!("DEVELOPER_DIR={}/Contents/Developer", dest.display());
        run(
            "sudo",
            &["env", &developer, path_text(&build)?, "-license", "accept"],
        )?;
        run(
            "sudo",
            &["env", &developer, path_text(&build)?, "-runFirstLaunch"],
        )?;
    }
    if let Some(version) = golden
        .default_xcode
        .as_deref()
        .or_else(|| golden.xcodes.first().map(|xcode| xcode.version.as_str()))
    {
        let developer = canonical_app(applications, version).join("Contents/Developer");
        run(
            "sudo",
            &["xcode-select", "--switch", path_text(&developer)?],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_match_only_at_component_boundaries() {
        assert!(version_matches("27.0", "27"));
        assert!(!version_matches("27.0.1", "27.0"));
        assert!(version_matches("26.5", "26.5"));
        assert!(!version_matches("27.1", "27.0"));
        assert!(!version_matches("27.10", "27.1"));
    }
    fn fake_app(path: &Path, version: &str) {
        fs::create_dir_all(path.join("Contents/Developer/usr/bin")).unwrap();
        fs::write(path.join("Contents/Developer/usr/bin/xcodebuild"), "").unwrap();
        fs::write(
            path.join("Contents/Info.plist"),
            format!("{{\"CFBundleShortVersionString\":\"{version}\"}}"),
        )
        .unwrap();
    }

    #[test]
    fn installs_two_versions_and_selects_the_named_default() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        fs::create_dir(&apps).unwrap();
        fake_app(&apps.join("Xcode.app"), "27.0");
        let archive = dir.path().join("bakery-xcode-26.5.tar");
        fs::write(&archive, "archive").unwrap();
        let golden: GoldenConfig = serde_json::from_str(
            r#"{"xcodes":[{"version":"27"},{"version":"26.5"}],"default_xcode":"26.5"}"#,
        )
        .unwrap();
        let mut calls = Vec::new();
        install(&golden, &apps, dir.path(), &mut |program, args| {
            calls.push((program.to_owned(), args.join(" ")));
            if program == "plutil" {
                return Ok(fs::read_to_string(args[4])?);
            }
            match args[0] {
                "mv" => fs::rename(args[1], args[2])?,
                "mkdir" => fs::create_dir(args[1])?,
                "tar" => fake_app(Path::new(args[2]), "26.5"),
                _ => {}
            }
            Ok(String::new())
        })
        .unwrap();
        assert!(apps.join("Xcode-27.app").is_dir());
        assert!(apps.join("Xcode-26.5.app").is_dir());
        assert!(!archive.exists());
        assert_eq!(
            calls.last().unwrap().1,
            format!(
                "xcode-select --switch {}/Xcode-26.5.app/Contents/Developer",
                apps.display()
            )
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(_, args)| args.ends_with("-runFirstLaunch"))
                .count(),
            2
        );
    }

    #[test]
    fn archive_version_mismatch_stops_before_xcode_selection() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("Applications");
        fs::create_dir(&apps).unwrap();
        let golden: GoldenConfig =
            serde_json::from_str(r#"{"xcodes":[{"version":"26.5"}]}"#).unwrap();
        let result = install(&golden, &apps, dir.path(), &mut |program, args| {
            if program == "plutil" {
                return Ok(fs::read_to_string(args[4])?);
            }
            match args[0] {
                "mkdir" => fs::create_dir(args[1])?,
                "tar" => fake_app(Path::new(args[2]), "27.0"),
                _ => panic!("Cannot select an Xcode version after an archive mismatch"),
            }
            Ok(String::new())
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not contain Xcode 26.5"));
    }
}
