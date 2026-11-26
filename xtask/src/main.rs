//! Developer tasks for the agito workspace.
//!
//! `cargo xtask codegen` regenerates the CDP bindings in `crates/agito-protocol/src/cdp`
//! from the vendored protocol JSON in `crates/agito-protocol/protocol`.

mod codegen;
mod naming;
mod schema;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("codegen") => match run_codegen(args.iter().any(|a| a == "--check")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("codegen failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: cargo xtask codegen [--check]");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn run_codegen(check: bool) -> Result<(), String> {
    let root = workspace_root();
    let protocol_dir = root.join("crates/agito-protocol/protocol");
    let out_dir = root.join("crates/agito-protocol/src/cdp");

    let mut domains = Vec::new();
    for file in ["js_protocol.json", "browser_protocol.json"] {
        let path = protocol_dir.join(file);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let protocol: schema::Protocol =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        domains.extend(protocol.domains);
    }

    let files = codegen::generate(&domains);

    if check {
        let mut stale = Vec::new();
        for (name, contents) in &files {
            let formatted = rustfmt(contents)?;
            let existing = std::fs::read_to_string(out_dir.join(name)).unwrap_or_default();
            if existing != formatted {
                stale.push(name.clone());
            }
        }
        if stale.is_empty() {
            println!("CDP bindings are up to date");
            return Ok(());
        }
        return Err(format!("stale generated files: {}", stale.join(", ")));
    }

    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    for (name, contents) in &files {
        let formatted = rustfmt(contents)?;
        std::fs::write(out_dir.join(name), formatted).map_err(|e| e.to_string())?;
    }
    println!("wrote {} files to {}", files.len(), out_dir.display());
    Ok(())
}

fn rustfmt(source: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run rustfmt: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(source.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| e.to_string())
}
