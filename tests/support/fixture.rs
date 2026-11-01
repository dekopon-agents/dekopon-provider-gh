pub fn build(example: Option<&str>) -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("target/test-fixtures");
    let mut flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_else(|_| {
        std::env::var("RUSTFLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("\x1f")
    });
    if !flags.is_empty() {
        flags.push('\x1f');
    }
    flags.push_str("--cfg=gh_foundation_fixture");
    let mut command = std::process::Command::new("cargo");
    command
        .env("CARGO_ENCODED_RUSTFLAGS", flags)
        .current_dir(&root)
        .args([
            "build",
            "--locked",
            "--manifest-path",
            "tests/fixtures/Cargo.toml",
            "--target",
            "wasm32-unknown-unknown",
            "--release",
            "--target-dir",
        ])
        .arg(&target);
    if let Some(name) = example {
        command.args(["--example", name]);
    }
    let status = command.status().expect("build isolated test fixture");
    assert!(status.success(), "fixture build failed");
    let artifact = example
        .map(|name| format!("examples/{name}"))
        .unwrap_or_else(|| "gh_foundation_fixture".into());
    let core = target.join(format!("wasm32-unknown-unknown/release/{artifact}.wasm"));
    let component = target.join(format!("{}.wasm", example.unwrap_or("foundation-fixture")));
    let status = std::process::Command::new("wasm-tools")
        .args(["component", "new"])
        .arg(core)
        .arg("-o")
        .arg(&component)
        .status()
        .expect("encode foundation fixture");
    assert!(status.success(), "fixture encoding failed");
    component
}
