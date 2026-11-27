//! Developer tasks for the agito workspace.
//!
//! - `cargo xtask codegen [--check]` regenerates the CDP bindings in
//!   `crates/agito-protocol/src/cdp` from the vendored protocol JSON in
//!   `crates/agito-protocol/protocol`, or checks that they are current.
//! - `cargo xtask update-protocol [--version <npm version>] [--dry-run]` vendors a
//!   `devtools-protocol` release from npm (`latest` by default), shows how it differs, then
//!   regenerates the bindings and checks that the workspace still builds.

mod codegen;
mod naming;
mod schema;
mod update_protocol;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask codegen [--check]
       cargo xtask update-protocol [--version <npm version>] [--dry-run]";

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
        Some("update-protocol") => match parse_update_args(&args[1..]).and_then(|o| run_update(&o))
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("update-protocol failed: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn parse_update_args(args: &[String]) -> Result<update_protocol::Options, String> {
    let mut options = update_protocol::Options {
        version: "latest".into(),
        dry_run: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dry-run" => options.dry_run = true,
            "--version" => {
                options.version = args
                    .next()
                    .ok_or_else(|| format!("--version needs a value\n{USAGE}"))?
                    .clone()
            }
            other => return Err(format!("unknown argument '{other}'\n{USAGE}")),
        }
    }
    Ok(options)
}

fn run_update(options: &update_protocol::Options) -> Result<(), String> {
    let root = workspace_root();
    let out_dir = root.join("crates/agito-protocol/src/cdp");
    let outcome = update_protocol::run(options, &root.join("crates/agito-protocol/protocol"))?;
    if !matches!(
        outcome,
        update_protocol::Outcome::Updated | update_protocol::Outcome::UpdatedWithoutChanges
    ) {
        return Ok(());
    }

    let before = read_dir_texts(&out_dir);
    run_codegen(false)?;
    let after = read_dir_texts(&out_dir);
    let changed: Vec<&String> = before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|name| before.get(*name) != after.get(*name))
        .collect();
    if changed.is_empty() {
        println!("The generated bindings did not change.");
        return Ok(());
    }
    println!(
        "Regenerated bindings changed in {} file(s): {}",
        changed.len(),
        changed
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // The bindings compile by construction; the question is whether the engine's use of them
    // still does, since a renamed field or a removed command breaks it here, not at runtime.
    println!("Checking that the workspace still builds...");
    let status = std::process::Command::new(env!("CARGO"))
        .args(["check", "--workspace", "--all-targets", "--quiet"])
        .current_dir(&root)
        .status()
        .map_err(|e| format!("could not run cargo check: {e}"))?;
    if !status.success() {
        return Err(
            "the workspace no longer builds against the new protocol; the errors above show \
             where the engine uses what changed"
                .into(),
        );
    }
    println!("The workspace builds. Run the browser tests before committing.");
    Ok(())
}

fn read_dir_texts(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some((e.file_name().to_string_lossy().into_owned(), text))
        })
        .collect()
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
