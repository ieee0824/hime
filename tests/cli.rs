use std::{fs, process::Command};
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hime"))
}
#[test]
fn fixture_json_and_effect_exit_code() {
    let output = cli()
        .args(["--json", "fixtures/effects.rs"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reports.as_array().unwrap().len(), 7);
    assert_eq!(reports[0]["status"], "candidate");
    assert_eq!(reports[3]["status"], "impure");
}
#[test]
fn strict_unknown_and_input_errors() {
    let path = std::env::temp_dir().join(format!("hime-cli-{}.rs", std::process::id()));
    fs::write(&path, "fn f() { external(); }").unwrap();
    assert_eq!(cli().arg(&path).status().unwrap().code(), Some(0));
    assert_eq!(
        cli().arg("--strict").arg(&path).status().unwrap().code(),
        Some(1)
    );
    fs::write(&path, "fn {").unwrap();
    assert_eq!(cli().arg(&path).status().unwrap().code(), Some(2));
    fs::remove_file(path).unwrap();
    assert_eq!(cli().arg("--invalid").status().unwrap().code(), Some(2));
}
#[test]
fn duplicate_paths_and_skipped_inputs() {
    let output = cli()
        .args(["fixtures", "./fixtures/effects.rs"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("7 functions"), "{text}");
    let output = cli().arg("Cargo.toml").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("skipping non-Rust file Cargo.toml")
    );
}
