//! Running a test binary on a set of its tests and reading what happened from libtest's output.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use agito::harness::{TestRecord, artifact_name, report::RESULTS_DIR};
use anyhow::{Context, Result};

use super::discover::{Binary, TestCase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
}

/// What happened to one test, across the runner's attempts.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub full_name: String,
    pub binary: String,
    pub name: String,
    pub status: Status,
    /// Known for `#[agito::test]` tests, which time themselves.
    pub duration: Option<Duration>,
    /// Runs of the test by `agito run`: more than one when it was retried.
    pub attempts: u32,
    /// Passed, but only on a retry.
    pub flaky: bool,
    /// Why the test is quarantined; its failure does not fail the run.
    pub quarantine: Option<String>,
    /// A one-line reason for a failure.
    pub message: Option<String>,
    /// What the test printed, as libtest captured it, for a failure.
    pub output: Option<String>,
    pub screenshot: Option<PathBuf>,
    /// The trace saved when the test failed.
    pub trace: Option<PathBuf>,
    /// What automatic accessibility audits found during the test.
    pub accessibility: Vec<agito::accessibility::Violation>,
}

impl Outcome {
    pub fn skipped(test: &TestCase) -> Outcome {
        Outcome::new(test, Status::Skipped)
    }

    fn new(test: &TestCase, status: Status) -> Outcome {
        Outcome {
            full_name: test.full_name(),
            binary: test.binary.id.clone(),
            name: test.name.clone(),
            status,
            duration: None,
            attempts: 1,
            flaky: false,
            quarantine: None,
            message: None,
            output: None,
            screenshot: None,
            trace: None,
            accessibility: Vec::new(),
        }
    }

    /// A failure that fails the run: not quarantined.
    pub fn gating_failure(&self) -> bool {
        self.status == Status::Failed && self.quarantine.is_none()
    }
}

/// How to run the binaries.
pub struct Settings {
    /// `--test-threads`; libtest's default when `None`.
    pub test_threads: Option<usize>,
    pub include_ignored: bool,
    /// A directory this run owns, for the harness's result records.
    pub results_root: PathBuf,
    /// `--a11y`: the accessibility mode for every test, overriding the configuration.
    pub accessibility: Option<String>,
}

/// Arguments beyond this length are split over several runs of a binary, well within
/// Windows' 32K-character command line.
const MAX_NAMES_LENGTH: usize = 16_000;

/// Runs `tests`, all from `binary`, calling `on_outcome` as each finishes. `whole` says that
/// they are every test the binary runs by default, so no names need passing.
pub fn run(
    binary: &Binary,
    tests: &[&TestCase],
    whole: bool,
    settings: &Settings,
    on_outcome: &mut dyn FnMut(&mut Outcome),
) -> Result<Vec<Outcome>> {
    if whole {
        return run_once(binary, tests, None, settings, on_outcome);
    }
    let mut outcomes = Vec::new();
    let mut chunk: Vec<&TestCase> = Vec::new();
    let mut length = 0;
    for test in tests {
        if length + test.name.len() > MAX_NAMES_LENGTH && !chunk.is_empty() {
            outcomes.extend(run_once(
                binary,
                &chunk,
                Some(&chunk),
                settings,
                on_outcome,
            )?);
            chunk.clear();
            length = 0;
        }
        length += test.name.len() + 1;
        chunk.push(test);
    }
    if !chunk.is_empty() {
        outcomes.extend(run_once(
            binary,
            &chunk,
            Some(&chunk),
            settings,
            on_outcome,
        )?);
    }
    Ok(outcomes)
}

fn run_once(
    binary: &Binary,
    expected: &[&TestCase],
    names: Option<&[&TestCase]>,
    settings: &Settings,
    on_outcome: &mut dyn FnMut(&mut Outcome),
) -> Result<Vec<Outcome>> {
    let results_dir = settings.results_root.join(artifact_name(&binary.id));
    let _ = std::fs::remove_dir_all(&results_dir);

    let mut command = binary.command();
    command.env(RESULTS_DIR, &results_dir);
    if let Some(mode) = &settings.accessibility {
        command.env("AGITO_ACCESSIBILITY_MODE", mode);
    }
    if let Some(threads) = settings.test_threads {
        command.arg(format!("--test-threads={threads}"));
    }
    if settings.include_ignored {
        command.arg("--include-ignored");
    }
    if let Some(names) = names {
        command.arg("--exact");
        command.args(names.iter().map(|t| &t.name));
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {}", binary.path.display()))?;

    // stderr is drained alongside, so a chatty test cannot fill the pipe and stall the binary.
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let by_name: HashMap<&str, &TestCase> =
        expected.iter().map(|t| (t.name.as_str(), *t)).collect();
    let mut outcomes: HashMap<String, Outcome> = HashMap::new();
    let mut parser = Parser::default();
    let stdout = child.stdout.take().expect("piped stdout");
    for line in std::io::BufReader::new(stdout).lines() {
        let line = line.context("could not read the test binary's output")?;
        if let Some((name, status)) = parser.feed(&line)
            && let Some(test) = by_name.get(name.as_str())
        {
            let mut outcome = Outcome::new(test, status);
            apply_record(&mut outcome, &results_dir, binary);
            on_outcome(&mut outcome);
            outcomes.insert(name, outcome);
        }
    }
    let status = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    let _ = std::fs::remove_dir_all(&results_dir);

    // Output for failures arrives after every result line, in libtest's summary.
    for (name, output) in parser.failure_output {
        if let Some(outcome) = outcomes.get_mut(&name) {
            // libtest separates the blocks with blank lines, which are not the test's.
            let output = format!("{}\n", output.trim_end());
            outcome.message = outcome.message.take().or_else(|| panic_message(&output));
            outcome.output = Some(output);
        }
    }

    let mut all: Vec<Outcome> = Vec::new();
    let unreported: Vec<&TestCase> = expected
        .iter()
        .copied()
        .filter(|t| !outcomes.contains_key(&t.name))
        .collect();
    all.extend(outcomes.into_values());
    if unreported.is_empty() {
        return Ok(all);
    }

    // The binary ran to the end, so a test with no result line printed over it, and libtest's
    // closing list of failures settles how it went.
    if parser.finished {
        for test in unreported {
            let status = if parser.failed_names.contains(&test.name) {
                Status::Failed
            } else {
                Status::Passed
            };
            let mut outcome = Outcome::new(test, status);
            on_outcome(&mut outcome);
            all.push(outcome);
        }
        return Ok(all);
    }

    // The binary died. Several tests were in flight on libtest's threads, and any of them may
    // have killed it, so each is run again alone; the one that dies alone is the culprit.
    if unreported.len() > 1 {
        for test in unreported {
            all.extend(run_once(
                binary,
                &[test],
                Some(&[test]),
                settings,
                on_outcome,
            )?);
        }
        return Ok(all);
    }
    let mut outcome = Outcome::new(unreported[0], Status::Failed);
    outcome.message = Some(format!(
        "the test binary died while running this test ({status})"
    ));
    let stderr = tail(&stderr, 40);
    outcome.output = (!stderr.trim().is_empty()).then_some(stderr);
    on_outcome(&mut outcome);
    all.push(outcome);
    Ok(all)
}

/// Fills in what the harness recorded for an `#[agito::test]`. The record is written before
/// libtest prints the result, so it is there to read.
fn apply_record(outcome: &mut Outcome, results_dir: &Path, binary: &Binary) {
    let file = results_dir.join(format!(
        "{}.json",
        artifact_name(&format!("{}::{}", binary.crate_name, outcome.name))
    ));
    let Some(record) = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TestRecord>(&bytes).ok())
    else {
        return;
    };
    outcome.duration = Some(Duration::from_secs_f64(record.duration_ms / 1000.0));
    outcome.screenshot = record.screenshot;
    outcome.trace = record.trace;
    outcome.message = record.message;
    outcome.accessibility = record.accessibility;
    if let Some(reason) = record.quarantine {
        outcome.quarantine = Some(reason);
        // libtest saw a pass: the harness does not fail a quarantined test.
        if !record.passed {
            outcome.status = Status::Failed;
        }
    }
}

/// The message of the first panic in a failure's output.
fn panic_message(output: &str) -> Option<String> {
    let mut lines = output.lines();
    lines.find(|l| l.contains("panicked at"))?;
    let message: Vec<&str> = lines
        .take_while(|l| !l.starts_with("note:") && !l.starts_with("stack backtrace:"))
        .collect();
    let message = message.join("\n").trim().to_string();
    (!message.is_empty()).then_some(message)
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Reads libtest's pretty output a line at a time.
#[derive(Default)]
struct Parser {
    section: Section,
    /// The test whose captured output is being read.
    current: Option<String>,
    failure_output: Vec<(String, String)>,
    /// The names libtest lists as failed at the end.
    failed_names: HashSet<String>,
    /// The `test result:` line was seen: the binary ran to the end.
    finished: bool,
}

#[derive(Default, PartialEq)]
enum Section {
    #[default]
    Results,
    FailureOutput,
    FailureList,
}

impl Parser {
    /// Takes one line; returns a test's result when the line reports one.
    fn feed(&mut self, line: &str) -> Option<(String, Status)> {
        if line.starts_with("test result: ") {
            self.finished = true;
            return None;
        }
        match self.section {
            Section::Results => {
                if line == "failures:" {
                    self.section = Section::FailureOutput;
                    return None;
                }
                result_line(line)
            }
            Section::FailureOutput => {
                if line == "failures:" {
                    self.section = Section::FailureList;
                    self.current = None;
                } else if let Some(name) = line
                    .strip_prefix("---- ")
                    .and_then(|l| l.strip_suffix(" stdout ----"))
                {
                    self.failure_output.push((name.to_owned(), String::new()));
                    self.current = Some(name.to_owned());
                } else if self.current.is_some()
                    && let Some((_, output)) = self.failure_output.last_mut()
                {
                    output.push_str(line);
                    output.push('\n');
                }
                None
            }
            Section::FailureList => {
                if let Some(name) = line.strip_prefix("    ") {
                    self.failed_names.insert(name.trim().to_owned());
                }
                None
            }
        }
    }
}

/// `test <name> ... ok`, `... FAILED`, `... ignored` or `... ignored, <reason>`.
fn result_line(line: &str) -> Option<(String, Status)> {
    let rest = line.strip_prefix("test ")?;
    let (name, result) = rest.rsplit_once(" ... ")?;
    let status = match result {
        "ok" => Status::Passed,
        "FAILED" => Status::Failed,
        r if r == "ignored" || r.starts_with("ignored, ") => Status::Skipped,
        _ => return None,
    };
    Some((name.to_owned(), status))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "
running 4 tests
test a::passes ... ok
test a::skipped ... ignored, needs network
test a::fails ... FAILED
test a::noisy ... ok

failures:

---- a::fails stdout ----

thread 'a::fails' panicked at src/lib.rs:9:5:
assertion `left == right` failed
  left: 1
 right: 2
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- a::lost stdout ----
gone

failures:
    a::fails
    a::lost

test result: FAILED. 2 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s
";

    #[test]
    fn parses_results_output_and_the_failure_list() {
        let mut parser = Parser::default();
        let results: Vec<(String, Status)> =
            OUTPUT.lines().filter_map(|l| parser.feed(l)).collect();
        assert_eq!(
            results,
            [
                ("a::passes".into(), Status::Passed),
                ("a::skipped".into(), Status::Skipped),
                ("a::fails".into(), Status::Failed),
                ("a::noisy".into(), Status::Passed),
            ]
        );
        assert!(parser.finished);
        assert_eq!(
            parser.failed_names,
            HashSet::from(["a::fails".to_string(), "a::lost".to_string()])
        );
        let (name, output) = &parser.failure_output[0];
        assert_eq!(name, "a::fails");
        assert_eq!(
            panic_message(output).unwrap(),
            "assertion `left == right` failed\n  left: 1\n right: 2"
        );
        assert_eq!(parser.failure_output[1].1.trim_end(), "gone");
    }

    #[test]
    fn result_lines_need_a_known_result() {
        assert_eq!(
            result_line("test x ... ok"),
            Some(("x".into(), Status::Passed))
        );
        assert_eq!(
            result_line("test x ... has been running for over 60 seconds"),
            None
        );
        assert_eq!(result_line("running 3 tests"), None);
    }
}
