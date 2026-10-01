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

#[test]
fn issue_regressions_have_correct_strict_exit_codes() {
    let path = std::env::temp_dir().join(format!("hime-issue-cli-{}.rs", std::process::id()));
    for (source, expected_code, expected_status) in [
        (
            "pub enum E { A = { const fn n() -> isize { assert!(1 > 0); 1 } n() } } pub struct S([u8; { const fn m() -> usize { assert!(1 > 0); 3 } m() }]);",
            1,
            "impure",
        ),
        (
            "#![no_std] #![recursion_limit = \"256\"] #![cfg_attr(docsrs, feature(doc_cfg))] pub fn add(a: i32, b: i32) -> i32 { a + b }",
            0,
            "candidate",
        ),
        ("#![cfg(any())] fn f() {}", 1, "unknown"),
        (
            "mod m { pub struct W(pub i32); } use m::W; fn f() { W(1); }",
            0,
            "candidate",
        ),
        (
            "#[cfg(unix)] struct W(i32); #[cfg(not(unix))] struct W(i64); fn f() { W(1); }",
            0,
            "candidate",
        ),
    ] {
        fs::write(&path, source).unwrap();
        let output = cli()
            .args(["--strict", "--json"])
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected_code), "{source}");
        let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let reports = reports.as_array().unwrap();
        assert!(!reports.is_empty(), "{source}");
        assert!(
            reports.iter().all(|r| r["status"] == expected_status),
            "{source}"
        );
    }
    fs::remove_file(path).unwrap();
}
