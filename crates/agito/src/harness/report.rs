//! Per-test result records, written when `AGITO_RESULTS_DIR` is set. `agito run` sets it and
//! reads the records back for what libtest's output does not carry: durations, attempts,
//! quarantine, and the failure screenshot.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The variable naming the directory records are written to.
pub const RESULTS_DIR: &str = "AGITO_RESULTS_DIR";

/// The outcome of one `#[agito::test]`, after any retries the harness made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRecord {
    /// `module_path!()::name`: libtest's name for the test, prefixed by the crate's name.
    pub name: String,
    /// Whether the test passed, after any retries. A quarantined test that failed records `false`.
    pub passed: bool,
    /// Across every attempt, including setup.
    pub duration_ms: f64,
    /// How many times the harness ran the test: more than one after a lost browser.
    pub attempts: u32,
    /// Why the test failed.
    pub message: Option<String>,
    /// The failure was the browser going away.
    pub browser_lost: bool,
    /// Why the test is quarantined, if it is.
    pub quarantine: Option<String>,
    /// The screenshot saved when the test failed.
    pub screenshot: Option<PathBuf>,
    /// What automatic accessibility audits found during the test.
    #[serde(default)]
    pub accessibility: Vec<crate::accessibility::Violation>,
}

impl TestRecord {
    /// The test's name as libtest lists it, without the crate prefix `module_path!()` adds.
    pub fn libtest_name(&self) -> &str {
        self.name
            .split_once("::")
            .map_or(self.name.as_str(), |(_, rest)| rest)
    }

    /// Reads every record in `dir`, skipping files that are not records.
    pub fn read_all(dir: &Path) -> Vec<TestRecord> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
            .collect()
    }
}

/// Writes `record` to the results directory, if there is one. A record that cannot be written
/// is reported, not raised: the test's outcome stands either way.
pub(crate) fn write(record: &TestRecord) {
    let Some(dir) = std::env::var_os(RESULTS_DIR).filter(|d| !d.is_empty()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let file = dir.join(format!("{}.json", super::artifact_name(&record.name)));
    let written = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::write(
            &file,
            serde_json::to_vec(record).expect("records serialize"),
        )
    });
    if let Err(e) = written {
        eprintln!("agito: could not write the result of {}: {e}", record.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libtest_names_drop_the_crate() {
        let record = |name: &str| TestRecord {
            name: name.into(),
            passed: true,
            duration_ms: 0.0,
            attempts: 1,
            message: None,
            browser_lost: false,
            quarantine: None,
            screenshot: None,
            accessibility: Vec::new(),
        };
        assert_eq!(record("popups::closes").libtest_name(), "closes");
        assert_eq!(
            record("agito::harness::tests::x").libtest_name(),
            "harness::tests::x"
        );
    }
}
