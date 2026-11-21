use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

const FAKE_TOOL: &str = r##"#!/bin/bash
set -eu
name="${0##*/}"
line="$name"
for arg in "$@"; do line+=$'\t'"$arg"; done
printf '%s\n' "$line" >> "$CALL_LOG"
case "$name" in
  tart)
    if [[ "$1" == run ]]; then
      touch "$CALL_LOG.run-${!#}"
    elif [[ "$1" == ip ]]; then
      for _ in {1..100}; do
        [[ -f "$CALL_LOG.run-$2" ]] && break
        /bin/sleep 0.01
      done
      [[ -f "$CALL_LOG.run-$2" ]]
      unlink "$CALL_LOG.run-$2"
      echo 192.0.2.1
    fi
    ;;
  ssh)
    command="${!#}"
    if [[ "$command" == /tmp/bakery\ xcodes\ contains* && "$command" == *"'${MISSING_XCODE:-none}'"* ]]; then
      exit 1
    elif [[ "$command" == *'xcrun simctl list runtimes -j' ]]; then
      printf '%s' "$GUEST_RUNTIMES"
    elif [[ "$command" == 'find /tmp/runtime'* ]]; then
      echo /tmp/runtime/export.dmg
    fi
    ;;
  xcrun)
    exit 1
    ;;
  scp)
    previous=""
    for arg in "$@"; do
      if [[ "$previous" == admin@* ]]; then printf 'runtime image' > "$arg"; fi
      previous="$arg"
    done
    ;;
esac
"##;

struct Workspace(tempfile::TempDir);

impl Workspace {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("scripts/bake")).unwrap();
        for name in ["bake-macos.sh", "bake/macos-install.sh"] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(name),
                root.join("scripts").join(name),
            )
            .unwrap();
        }
        fs::create_dir(root.join("bin")).unwrap();
        for name in ["tart", "ssh", "scp", "xcrun", "sleep"] {
            let tool = root.join("bin").join(name);
            fs::write(&tool, FAKE_TOOL).unwrap();
            fs::set_permissions(tool, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(directory)
    }

    fn bake(&self, specs: Value, runtimes: Value) -> Vec<Vec<String>> {
        self.bake_apple(specs, runtimes, json!({}), "none")
    }

    fn bake_apple(
        &self,
        specs: Value,
        runtimes: Value,
        apple: Value,
        missing: &str,
    ) -> Vec<Vec<String>> {
        let root = self.0.path();
        let path = std::env::join_paths(
            std::iter::once(root.join("bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = Command::new("bash")
            .arg(root.join("scripts/bake-macos.sh"))
            .current_dir(root)
            .env("PATH", path)
            .env("CALL_LOG", root.join("calls"))
            .env("BAKERY_SIMULATORS_JSON", specs.to_string())
            .env("BAKERY_APPLE_JSON", apple.to_string())
            .env("MISSING_XCODE", missing)
            .env("BAKERY_BIN", env!("CARGO_BIN_EXE_bakery"))
            .env("GUEST_RUNTIMES", json!({"runtimes": runtimes}).to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .map(|line| line.split('\t').map(str::to_owned).collect())
            .collect()
    }
}

#[test]
fn first_runtime_image_reaches_the_matching_guest_path() {
    let workspace = Workspace::new();
    let image = workspace.0.path().join("image with spaces.dmg");
    fs::write(&image, "image").unwrap();
    let calls = workspace.bake(
        json!([{"platform": "iOS", "version": "18.6", "runtime_image": image}]),
        json!([]),
    );
    assert!(calls.iter().any(|call| call[0] == "scp"
        && call[call.len() - 2] == image.to_str().unwrap()
        && call.last().unwrap().ends_with("/tmp/bakery-runtime-0.dmg")));
    assert!(calls.iter().any(|call| call[0] == "scp"
        && call[call.len() - 2] == env!("CARGO_BIN_EXE_bakery")
        && call.last().unwrap().ends_with("/tmp/bakery")));
}

#[test]
fn installed_runtime_skips_donor_and_image_transfer() {
    let workspace = Workspace::new();
    let calls = workspace.bake(json!([{"platform": "iOS", "version": "18.6", "donor_source": "donor-image"}]),
                               json!([{"identifier": "ios-18-6", "platform": "iOS", "version": "18.6", "isAvailable": true, "buildversion": "22G86"}]));
    assert!(!calls.iter().any(|call| call.starts_with(&[
        "tart".into(),
        "clone".into(),
        "donor-image".into()
    ])));
    assert!(!calls
        .iter()
        .any(|call| call[0] == "scp" && call.last().unwrap().contains("bakery-runtime-")));
}

#[test]
fn donor_download_is_cached_between_golden_boots() {
    let workspace = Workspace::new();
    let calls = workspace.bake(
        json!([{"platform": "iOS", "version": "18.6", "donor_source": "donor-image"}]),
        json!([]),
    );
    let cache = workspace
        .0
        .path()
        .join("state/ios-18.6-simulator-runtime.dmg");
    assert_eq!(fs::read_to_string(&cache).unwrap(), "runtime image");
    let donor = calls
        .iter()
        .position(|call| call.starts_with(&["tart".into(), "clone".into(), "donor-image".into()]))
        .unwrap();
    let shutdown = calls
        .iter()
        .position(|call| call[0] == "ssh" && call.last().unwrap() == "sudo shutdown -h now")
        .unwrap();
    let boots: Vec<_> = calls
        .iter()
        .enumerate()
        .filter(|(_, call)| {
            call.starts_with(&["tart".into(), "run".into(), "--no-graphics".into()])
                && call.last().unwrap() == "macos"
        })
        .map(|(index, _)| index)
        .collect();
    assert!(shutdown < donor);
    assert!(boots[1] > donor);
    assert!(calls.iter().any(|call| call[0] == "scp"
        && call[call.len() - 2] == cache.to_str().unwrap()
        && call.last().unwrap().ends_with("/tmp/bakery-runtime-0.dmg")));
}

#[test]
fn empty_selection_does_not_copy_or_download_runtimes() {
    let workspace = Workspace::new();
    let calls = workspace.bake(json!([]), json!([]));
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with(&["tart".into(), "clone".into()]))
            .count(),
        1
    );
    assert!(!calls
        .iter()
        .any(|call| call[0] == "scp" && call.last().unwrap().contains("bakery-runtime-")));
}

#[test]
fn additional_xcode_uses_its_donor_and_installs_before_simulator_setup() {
    let workspace = Workspace::new();
    let calls = workspace.bake_apple(
        json!([]),
        json!([]),
        json!({
            "xcodes": [{"version": "27"}, {"version": "26.5", "source": "older-xcode-image"}],
            "default_xcode": "27"
        }),
        "26.5",
    );
    let donor = calls
        .iter()
        .position(|call| {
            call.starts_with(&["tart".into(), "clone".into(), "older-xcode-image".into()])
        })
        .unwrap();
    let install = calls
        .iter()
        .position(|call| {
            call[0] == "ssh"
                && call.last().unwrap() == "/tmp/bakery xcodes install /tmp/bakery-apple.json"
        })
        .unwrap();
    let simulator = calls
        .iter()
        .position(|call| call[0] == "ssh" && call.last().unwrap().contains("for _ in $(seq 60)"))
        .unwrap();
    assert!(donor < install && install < simulator);
    assert!(workspace.0.path().join("state/xcode-26.5.tar").is_file());
    assert!(calls.iter().any(
        |call| call[0] == "scp" && call.last().unwrap().ends_with("/tmp/bakery-xcode-26.5.tar")
    ));
}

#[test]
fn cached_xcode_skips_donor_and_simulator_uses_selected_toolchain() {
    let workspace = Workspace::new();
    let state = workspace.0.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::write(state.join("xcode-26.5.tar"), "cached Xcode").unwrap();
    let calls = workspace.bake_apple(
        json!([{"platform": "iOS", "version": "18.6", "xcode": "26.5"}]),
        json!([]),
        json!({"xcodes": [{"version": "26.5", "source": "older-xcode-image"}]}),
        "26.5",
    );
    assert!(!calls.iter().any(|call| call.starts_with(&[
        "tart".into(),
        "clone".into(),
        "older-xcode-image".into()
    ])));
    assert!(calls.iter().any(|call| call[0] == "ssh" && call.last().unwrap() == "env DEVELOPER_DIR='/Applications/Xcode-26.5.app/Contents/Developer' xcrun simctl list runtimes -j"));
}
