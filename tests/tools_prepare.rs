use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn xcode_donor_transfer_cache_and_failure_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let script = r#"#!/bin/bash
set -eu
name="${0##*/}"
printf '%s %s\n' "$name" "$*" >> "$CALL_LOG"
case "$name" in
  tart) [[ "$1" != ip ]] || echo 192.0.2.2 ;;
  ssh)
    if [[ "${!#}" == *'xcodebuild -version'* ]]; then
      if [[ "$*" == *admin@192.0.2.1* ]]; then [[ -z "$HOST_VERSION" ]] || echo "Xcode $HOST_VERSION"; else echo "Xcode $DONOR_VERSION"; fi
    fi ;;
  scp) [[ "$*" != *admin@192.0.2.2:* ]] || touch "${!#}" ;;
esac
"#;
    for name in ["tart", "ssh", "scp"] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let manifest = root.join("tools.json");
    let log = root.join("calls");
    let run = |donor: &str, source: &str, active: &str| {
        Command::new(env!("CARGO_BIN_EXE_bakery"))
            .current_dir(root)
            .env("PATH", &path)
            .env("CALL_LOG", &log)
            .env("DONOR_VERSION", donor)
            .env("HOST_VERSION", active)
            .args([
                "prepare-tools",
                "192.0.2.1",
                manifest.to_str().unwrap(),
                source,
            ])
            .output()
            .unwrap()
    };
    fs::write(&manifest, json!({"xcodes": ["26.5"]}).to_string()).unwrap();
    let result = run("26.5", "default-root", "27.2");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(root.join("state/xcode-26.5.tar").exists());
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("tart clone ghcr.io/cirruslabs/macos-tahoe-xcode:26.5 bakery-xcode-donor-")
    );
    assert!(calls.contains("admin@192.0.2.1:/tmp/bakery-xcode-26.5.tar"));
    assert!(calls.contains("tart stop bakery-xcode-donor-"));
    assert!(calls.contains("tart delete bakery-xcode-donor-"));
    fs::write(&log, "").unwrap();
    assert!(run("wrong", "default-root", "27.2").status.success());
    assert!(!fs::read_to_string(&log).unwrap().contains("tart clone"));

    fs::write(&manifest, json!({"xcodes": ["26.6"]}).to_string()).unwrap();
    fs::write(&log, "").unwrap();
    let result = run("26.5", "default-root", "27.2");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("expected 26.6"));
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("tart delete bakery-xcode-donor-"));
    assert!(!calls.contains("scp "));

    fs::write(&manifest, "{}").unwrap();
    fs::write(&log, "").unwrap();
    assert!(run("27.2", "custom-root", "").status.success());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(manifest["xcodes"], json!(["27.2"]));
    assert!(fs::read_to_string(&log)
        .unwrap()
        .contains("ghcr.io/cirruslabs/macos-golden-gate-xcode:latest"));
}
