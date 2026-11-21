use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "bakery-build-test-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("scripts")).unwrap();
        for image in ["macos", "ubuntu"] {
            let script = root.join(format!("scripts/bake-{image}.sh"));
            fs::write(&script, "#!/bin/bash\nprintf '%s\\n%s\\n' \"$1\" \"${BAKERY_SIMULATORS_JSON:-}\" > received.txt\n").unwrap();
            fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(root)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bakery"))
            .current_dir(&self.0)
            .env_remove("BAKERY_SIMULATORS_JSON")
            .env_remove("BAKE_SOURCE")
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn custom_config_and_destination_select_the_matching_golden() {
    let workspace = Workspace::new();
    fs::write(workspace.0.join("custom.yaml"), "github: {org: test}\nrunners: []\ngoldens:\n  macos:\n    simulators: []\n  mobile:\n    simulators:\n      - {platform: iOS, version: '18.6'}\n").unwrap();
    let result = workspace.run(&["-c", "custom.yaml", "build", "macos", "--as", "mobile"]);
    assert!(result.status.success(), "{:?}", result);
    let received = fs::read_to_string(workspace.0.join("received.txt")).unwrap();
    let (destination, manifest) = received.split_once('\n').unwrap();
    assert_eq!(destination, "mobile");
    let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
    assert_eq!(manifest[0]["platform"], "iOS");
    assert_eq!(manifest[0]["version"], "18.6");
}

#[test]
fn omitted_golden_passes_an_empty_selection() {
    let workspace = Workspace::new();
    fs::write(
        workspace.0.join("runners.yaml"),
        "github: {org: test}\nrunners: []\n",
    )
    .unwrap();
    assert!(workspace.run(&["build", "macos"]).status.success());
    assert_eq!(
        fs::read_to_string(workspace.0.join("received.txt")).unwrap(),
        "macos\n[]\n"
    );
}

#[test]
fn macos_requires_valid_configuration_before_running_the_script() {
    let workspace = Workspace::new();
    assert!(!workspace.run(&["build", "macos"]).status.success());
    assert!(!workspace.0.join("received.txt").exists());
    fs::write(workspace.0.join("runners.yaml"), "github: {org: test}\nrunners: []\ngoldens: {macos: {simulators: [{platform: iOS, version: 'bad'}]}}\n").unwrap();
    assert!(!workspace.run(&["build", "macos"]).status.success());
    assert!(!workspace.0.join("received.txt").exists());
}

#[test]
fn listing_and_linux_builds_do_not_require_configuration() {
    let workspace = Workspace::new();
    assert!(workspace.run(&["build"]).status.success());
    assert!(workspace.run(&["build", "ubuntu"]).status.success());
}

#[test]
fn xcode_configuration_sets_base_image_and_reaches_the_bake() {
    let workspace = Workspace::new();
    fs::write(
        workspace.0.join("scripts/bake-macos.sh"),
        "#!/bin/bash\nprintf '%s\\n%s\\n' \"$BAKE_SOURCE\" \"$BAKERY_APPLE_JSON\" > received.txt\n",
    )
    .unwrap();
    fs::write(workspace.0.join("runners.yaml"), "github: {org: test}\nrunners: []\ngoldens:\n  macos:\n    xcodes: [{version: '27', source: 'custom-xcode-image'}, {version: '26.5'}]\n    default_xcode: '26.5'\n").unwrap();
    assert!(workspace.run(&["build", "macos"]).status.success());
    let received = fs::read_to_string(workspace.0.join("received.txt")).unwrap();
    let (source, manifest) = received.split_once('\n').unwrap();
    assert_eq!(source, "custom-xcode-image");
    let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
    assert_eq!(manifest["default_xcode"], "26.5");
    assert_eq!(manifest["xcodes"][1]["version"], "26.5");
}
