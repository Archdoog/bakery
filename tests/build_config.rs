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

    fn record_root(&self) {
        for image in ["macos", "ubuntu"] {
            fs::write(
                self.0.join(format!("scripts/bake-{image}.sh")),
                "#!/bin/bash\nprintf '%s' \"$BAKE_SOURCE\" > root.txt\n",
            )
            .unwrap();
        }
    }

    fn root(&self) -> String {
        fs::read_to_string(self.0.join("root.txt")).unwrap()
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
fn ephemeral_fleet_passes_xcode_configuration_to_bake() {
    let workspace = Workspace::new();
    fs::write(
        workspace.0.join("scripts/bake-macos.sh"),
        "#!/bin/bash\nprintf '%s\\n%s\\n' \"$BAKE_SOURCE\" \"$BAKERY_APPLE_JSON\" > received.txt\n",
    )
    .unwrap();
    fs::write(workspace.0.join("runners.yaml"), "github: {org: test}\nrunners:\n  - name: macos\n    kind: macos\n    image: macos\n    count: 1\n    ephemeral: true\n    resources: {cpu: 2, memory_gb: 4}\n    labels: [xcode]\ngoldens:\n  macos:\n    xcodes: [{version: '27', source: 'custom-xcode-image'}, {version: '26.5'}]\n    default_xcode: '26.5'\n").unwrap();
    assert!(workspace.run(&["build", "macos"]).status.success());
    let received = fs::read_to_string(workspace.0.join("received.txt")).unwrap();
    let (source, manifest) = received.split_once('\n').unwrap();
    assert_eq!(source, "ghcr.io/cirruslabs/macos-golden-gate-xcode:latest");
    let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
    assert_eq!(manifest["default_xcode"], "26.5");
    assert_eq!(manifest["xcodes"][1]["version"], "26.5");
    assert_eq!(manifest["xcodes"][0]["source"], "custom-xcode-image");
}

#[test]
fn root_images_apply_to_both_bakes() {
    let workspace = Workspace::new();
    workspace.record_root();
    fs::write(workspace.0.join("runners.yaml"), "github: {org: test}\nrunners: []\nroot_images: {macos: 'ghcr.io/example/macos', ubuntu: 'registry.example:5000/ubuntu'}\ngoldens: {macos: {xcodes: [{version: '27.2'}]}}\n").unwrap();
    assert!(workspace.run(&["build", "macos"]).status.success());
    assert_eq!(workspace.root(), "ghcr.io/example/macos:latest");
    assert!(workspace.run(&["build", "ubuntu"]).status.success());
    assert_eq!(workspace.root(), "registry.example:5000/ubuntu:latest");
}

#[test]
fn root_image_defaults_tags_digests_and_local_names_reach_the_script() {
    let workspace = Workspace::new();
    workspace.record_root();
    for (source, expected) in [
        (None, "ghcr.io/cirruslabs/macos-golden-gate-xcode:latest"),
        (Some("ghcr.io/example/macos"), "ghcr.io/example/macos:latest"),
        (Some("ghcr.io/example/macos:27.2"), "ghcr.io/example/macos:27.2"),
        (Some("ghcr.io/example/macos@sha256:0000000000000000000000000000000000000000000000000000000000000000"), "ghcr.io/example/macos@sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        (Some("local-root"), "local-root"),
    ] {
        let roots = source.map(|source| format!("root_images: {{macos: '{source}'}}\n")).unwrap_or_default();
        fs::write(workspace.0.join("runners.yaml"), format!("github: {{org: test}}\nrunners: []\n{roots}")).unwrap();
        assert!(workspace.run(&["build", "macos"]).status.success());
        assert_eq!(workspace.root(), expected);
    }
    fs::write(
        workspace.0.join("runners.yaml"),
        "github: {org: test}\nrunners: []\nroot_images: {macos: 'local-root'}\n",
    )
    .unwrap();
    assert!(workspace.run(&["build", "ubuntu"]).status.success());
    assert_eq!(workspace.root(), "ghcr.io/cirruslabs/ubuntu:latest");
}

#[test]
fn golden_override_and_environment_take_precedence_over_root_images() {
    let workspace = Workspace::new();
    workspace.record_root();
    fs::write(workspace.0.join("custom.yaml"), "github: {org: test}\nrunners: []\nroot_images: {ubuntu: 'ghcr.io/example/shared'}\ngoldens: {custom: {source: 'ghcr.io/example/override:24.04'}}\n").unwrap();
    assert!(workspace
        .run(&["-c", "custom.yaml", "build", "ubuntu", "--as", "custom"])
        .status
        .success());
    assert_eq!(workspace.root(), "ghcr.io/example/override:24.04");
    let result = Command::new(env!("CARGO_BIN_EXE_bakery"))
        .current_dir(&workspace.0)
        .env("BAKE_SOURCE", "environment-root")
        .args(["-c", "custom.yaml", "build", "ubuntu", "--as", "custom"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(workspace.root(), "environment-root");
}

#[test]
fn invalid_root_images_stop_before_the_bake_script() {
    let workspace = Workspace::new();
    for source in ["''", "\"bad\\nimage\""] {
        fs::write(
            workspace.0.join("runners.yaml"),
            format!("github: {{org: test}}\nrunners: []\nroot_images: {{ubuntu: {source}}}\n"),
        )
        .unwrap();
        let result = workspace.run(&["build", "ubuntu"]);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid root image"));
        assert!(!workspace.0.join("received.txt").exists());
    }
}

#[test]
fn configured_roots_reach_the_actual_bake_scripts() {
    let workspace = Workspace::new();
    let bin = workspace.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let tart = bin.join("tart");
    fs::write(
        &tart,
        "#!/bin/bash\n[[ \"$1\" == clone ]] || exit 43\nprintf '%s' \"$2\" > root.txt\nexit 42\n",
    )
    .unwrap();
    fs::set_permissions(&tart, fs::Permissions::from_mode(0o755)).unwrap();
    let sshpass = bin.join("sshpass");
    fs::write(&sshpass, "#!/bin/bash\nexit 1\n").unwrap();
    fs::set_permissions(sshpass, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(workspace.0.join("scripts/bake")).unwrap();
    fs::write(workspace.0.join("runners.yaml"), "github: {org: test}\nrunners: []\nroot_images: {macos: 'ghcr.io/example/macos', ubuntu: 'ghcr.io/example/ubuntu'}\n").unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for image in ["macos", "ubuntu"] {
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("scripts/bake-{image}.sh")),
            workspace.0.join(format!("scripts/bake-{image}.sh")),
        )
        .unwrap();
        fs::write(
            workspace.0.join(format!("scripts/bake/{image}-install.sh")),
            "",
        )
        .unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_bakery"))
            .current_dir(&workspace.0)
            .env("PATH", &path)
            .env_remove("BAKE_SOURCE")
            .args(["build", image])
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(42),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(workspace.root(), format!("ghcr.io/example/{image}:latest"));
    }
}
