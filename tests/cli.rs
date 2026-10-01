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
