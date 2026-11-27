//! Building the test binaries with cargo and listing the tests in each.

use std::ffi::OsString;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// A test binary cargo built: a library's unit tests, an integration test target, and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binary {
    /// `package` for a library's unit tests, `package::target` for an integration test, and
    /// `package::bin/target` (or `example/`, `bench/`) for the others, as cargo-nextest names
    /// them.
    pub id: String,
    pub package: String,
    /// The crate's name, as `module_path!()` begins: the target's name with `-` as `_`.
    pub crate_name: String,
    pub path: PathBuf,
    /// The package's directory, where `cargo test` runs a binary.
    pub manifest_dir: PathBuf,
    /// The dynamic library search path to run it with; see [`library_path`].
    pub library_path: Option<OsString>,
}

impl Binary {
    /// A command for this binary, set up as `cargo test` would run it.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command
            .current_dir(&self.manifest_dir)
            .env("CARGO_MANIFEST_DIR", &self.manifest_dir)
            .env("CARGO_PKG_NAME", &self.package)
            .stdin(Stdio::null());
        if let Some(path) = &self.library_path {
            command.env(LIBRARY_PATH_VAR, path);
        }
        command
    }
}

/// One test in a binary.
#[derive(Debug, Clone)]
pub struct TestCase {
    pub binary: Arc<Binary>,
    /// libtest's name for it, such as `tests::parses`.
    pub name: String,
    /// `#[ignore]`d.
    pub ignored: bool,
}

impl TestCase {
    /// The name used in reports, quarantine lists and flaky history: `binary::test`.
    pub fn full_name(&self) -> String {
        format!("{}::{}", self.binary.id, self.name)
    }
}

/// Runs `cargo test --no-run` with `cargo_args` and returns the test binaries it built.
/// Compiler output goes to the terminal as `cargo test` shows it.
pub fn build(cargo_args: &[String]) -> Result<Vec<Binary>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut child = Command::new(&cargo)
        .args([
            "test",
            "--no-run",
            "--message-format",
            "json-render-diagnostics",
        ])
        .args(cargo_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not run cargo")?;
    let mut binaries: Vec<Binary> = Vec::new();
    let stdout = child.stdout.take().expect("piped stdout");
    for line in std::io::BufReader::new(stdout).lines() {
        let line = line.context("could not read cargo's output")?;
        if let Some(binary) = serde_json::from_str::<Value>(&line)
            .ok()
            .as_ref()
            .and_then(test_binary)
            && !binaries.iter().any(|b| b.path == binary.path)
        {
            binaries.push(binary);
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("cargo could not build the tests");
    }
    binaries.sort_by(|a, b| a.id.cmp(&b.id));
    let toolchain_libs = target_libdir();
    for binary in &mut binaries {
        binary.library_path = library_path(toolchain_libs.as_deref(), &binary.path);
    }
    Ok(binaries)
}

/// The variable the platform's loader searches for shared libraries.
const LIBRARY_PATH_VAR: &str = if cfg!(windows) {
    "PATH"
} else if cfg!(target_os = "macos") {
    "DYLD_FALLBACK_LIBRARY_PATH"
} else {
    "LD_LIBRARY_PATH"
};

/// The toolchain's library directory, where the shared standard library lives.
fn target_libdir() -> Option<PathBuf> {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "target-libdir"])
        .output()
        .ok()?;
    let dir = String::from_utf8(output.stdout).ok()?;
    output.status.success().then(|| PathBuf::from(dir.trim()))
}

/// The search path `cargo test` gives a binary: the toolchain's libraries, which a
/// proc-macro crate's tests link dynamically, and the binary's own directory, for dylib
/// dependencies, ahead of what the variable already holds.
fn library_path(toolchain: Option<&Path>, binary: &Path) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = toolchain.into_iter().map(Path::to_path_buf).collect();
    dirs.extend(binary.parent().map(Path::to_path_buf));
    if let Some(existing) = std::env::var_os(LIBRARY_PATH_VAR) {
        dirs.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(dirs).ok()
}

/// The test binary a `compiler-artifact` message describes, if it describes one.
fn test_binary(message: &Value) -> Option<Binary> {
    if message.get("reason")?.as_str()? != "compiler-artifact"
        || message.pointer("/profile/test")?.as_bool() != Some(true)
    {
        return None;
    }
    let path = PathBuf::from(message.get("executable")?.as_str()?);
    let target = message.get("target")?;
    let target_name = target.get("name")?.as_str()?;
    let kind = target.pointer("/kind/0")?.as_str()?;
    let package = package_name(message.get("package_id")?.as_str()?)?;
    let manifest_dir = Path::new(message.get("manifest_path")?.as_str()?)
        .parent()?
        .to_path_buf();
    let id = match kind {
        "test" => format!("{package}::{target_name}"),
        "bin" | "example" | "bench" => format!("{package}::{kind}/{target_name}"),
        _ => package.clone(),
    };
    Some(Binary {
        id,
        package,
        crate_name: target_name.replace('-', "_"),
        path,
        manifest_dir,
        library_path: None,
    })
}

/// The package's name from a package id, which is either `<url>#<name>@<version>` or, when the
/// name matches the directory, `<url>#<version>`.
fn package_name(package_id: &str) -> Option<String> {
    let (url, fragment) = package_id.rsplit_once('#')?;
    if let Some((name, _version)) = fragment.split_once('@') {
        return Some(name.to_string());
    }
    Some(url.trim_end_matches('/').rsplit('/').next()?.to_string())
}

/// Lists the tests in `binary`, marking the ignored ones.
pub fn list(binary: &Arc<Binary>) -> Result<Vec<TestCase>> {
    let names = |ignored: bool| -> Result<Vec<String>> {
        let mut command = binary.command();
        command.args(["--list", "--format", "terse"]);
        if ignored {
            command.arg("--ignored");
        }
        let output = command
            .output()
            .with_context(|| format!("could not run {}", binary.path.display()))?;
        if !output.status.success() {
            bail!(
                "{} could not list its tests: {}",
                binary.id,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(parse_list(&String::from_utf8_lossy(&output.stdout)))
    };
    let ignored = names(true)?;
    Ok(names(false)?
        .into_iter()
        .map(|name| TestCase {
            binary: binary.clone(),
            ignored: ignored.contains(&name),
            name,
        })
        .collect())
}

/// The test names in `--list --format terse` output; benchmarks are left out.
fn parse_list(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn artifact(kind: &str, name: &str, package_id: &str) -> Value {
        json!({
            "reason": "compiler-artifact",
            "package_id": package_id,
            "manifest_path": "/w/crates/agito/Cargo.toml",
            "target": { "kind": [kind], "name": name },
            "profile": { "test": true },
            "executable": format!("/w/target/debug/deps/{name}-abc"),
        })
    }

    #[test]
    fn binaries_are_named_as_nextest_names_them() {
        let id = |kind, name| {
            test_binary(&artifact(kind, name, "path+file:///w/crates/agito#0.2.0"))
                .unwrap()
                .id
        };
        assert_eq!(id("lib", "agito"), "agito");
        assert_eq!(id("proc-macro", "agito"), "agito");
        assert_eq!(id("test", "popups"), "agito::popups");
        assert_eq!(id("bin", "agito"), "agito::bin/agito");

        let binary = test_binary(&artifact(
            "test",
            "live-transport",
            "path+file:///w/crates/agito#0.2.0",
        ))
        .unwrap();
        assert_eq!(binary.crate_name, "live_transport");
        assert_eq!(binary.manifest_dir, Path::new("/w/crates/agito"));
    }

    #[test]
    fn only_test_artifacts_count() {
        let mut lib = artifact("lib", "agito", "path+file:///w/crates/agito#0.2.0");
        lib["profile"]["test"] = json!(false);
        assert!(test_binary(&lib).is_none());
        let mut no_exe = artifact("lib", "agito", "path+file:///w/crates/agito#0.2.0");
        no_exe["executable"] = Value::Null;
        assert!(test_binary(&no_exe).is_none());
        assert!(test_binary(&json!({ "reason": "build-finished", "success": true })).is_none());
    }

    #[test]
    fn package_names_come_from_either_id_form() {
        assert_eq!(
            package_name("path+file:///w/crates/agito#0.2.0").as_deref(),
            Some("agito")
        );
        assert_eq!(
            package_name("path+file:///w/samples/samples#agito-samples@0.0.0").as_deref(),
            Some("agito-samples")
        );
        assert_eq!(
            package_name("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0")
                .as_deref(),
            Some("serde")
        );
    }

    #[test]
    fn list_output_keeps_tests_only() {
        let out = "tests::a: test\ntests::b: test\nbench_x: bench\n";
        assert_eq!(parse_list(out), ["tests::a", "tests::b"]);
    }
}
