use std::process::Command;

#[test]
fn guest_readiness_regressions() {
    assert!(Command::new("python3")
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests",
            "-p",
            "test_guest_readiness.py"
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .unwrap()
        .success());
}
