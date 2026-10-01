use hime::{Status, analyze_source};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

fn files(path: &Path, result: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Do not follow symlinks, which could cycle or escape the requested tree.
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for entry in entries {
            if entry
                .file_name()
                .is_some_and(|s| s == "target" || s == ".git")
            {
                continue;
            }
            files(&entry, result)?;
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        result.push(path.to_owned());
    }
    Ok(())
}
fn run() -> Result<u8, String> {
    let mut json = false;
    let mut strict = false;
    let mut paths = vec![];
    let mut positional = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--" if !positional => positional = true,
            "--json" if !positional => json = true,
            "--strict" if !positional => strict = true,
            "--help" | "-h" if !positional => {
                println!(
                    "hime [--json] [--strict] [PATH ...]\n\nAnalyze Rust files or directories (default: src).\nStatuses: candidate / unknown / impure. Candidate is not a purity proof.\nExit: 0 = no detected effects; 1 = impure (or unknown with --strict); 2 = input error.\nImports, types, macros and cross-file calls are not resolved."
                );
                return Ok(0);
            }
            _ if !positional && arg.starts_with('-') => {
                return Err(format!("unknown option: {arg}"));
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    if paths.is_empty() {
        paths.push(PathBuf::from("src"));
    }
    let mut inputs = vec![];
    for path in paths {
        files(&path, &mut inputs)?;
    }
    inputs.sort();
    inputs.dedup();
    if inputs.is_empty() {
        return Err("no Rust source files found".into());
    }
    let mut reports = vec![];
    for file in inputs {
        let source = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        reports.extend(
            analyze_source(&file.display().to_string(), &source).map_err(|e| {
                format!(
                    "{}:{}:{}: {e}",
                    file.display(),
                    e.span().start().line,
                    e.span().start().column + 1
                )
            })?,
        );
    }
    let failed = reports
        .iter()
        .any(|r| r.status == Status::Impure || strict && r.status == Status::Unknown);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&reports).map_err(|e| e.to_string())?
        );
    } else {
        for report in &reports {
            println!(
                "{}:{}: {} [{:?}]",
                report.file, report.line, report.function, report.status
            );
            for d in &report.diagnostics {
                println!(
                    "  {}:{}:{}: {}: {}",
                    report.file, d.line, d.column, d.code, d.message
                );
            }
        }
        let count = |s| reports.iter().filter(|r| r.status == s).count();
        println!(
            "{} functions: {} candidate, {} unknown, {} impure",
            reports.len(),
            count(Status::Candidate),
            count(Status::Unknown),
            count(Status::Impure)
        );
    }
    Ok(u8::from(failed))
}
fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("hime: {e}");
            ExitCode::from(2)
        }
    }
}
