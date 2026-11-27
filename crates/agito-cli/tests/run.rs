//! Runs `agito run` on `tests/fixture`, a suite that misbehaves on purpose.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "agito-run-e2e-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `agito run <args>` on the fixture, with `state` for the flaky test's marker.
fn run(state: &Path, args: &[&str]) -> (Output, String) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/Cargo.toml");
    // `run`'s own options must come before the cargo arguments, so the manifest goes between
    // them and the test arguments after `--`.
    let (options, tests) = match args.iter().position(|a| *a == "--") {
        Some(at) => (&args[..at], &args[at..]),
        None => (args, &[][..]),
    };
    let mut command_args: Vec<&str> = options.to_vec();
    command_args.extend(["--manifest-path", fixture.to_str().unwrap()]);
    command_args.extend(tests);
    let output = Command::new(env!("CARGO_BIN_EXE_agito"))
        .arg("run")
        .args(&command_args)
        .env("CARGO_TARGET_DIR", workspace().join("target/run-fixture"))
        .env("FIXTURE_STATE", state)
        .env_remove("FIXTURE_CRASH")
        .output()
        .expect("agito runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !stdout.contains("Results:") {
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    (output, stdout)
}

/// The names `agito run` printed with `label`, such as `PASS`.
fn labelled(stdout: &str, label: &str) -> Vec<String> {
    let mut names: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix(label))
        .map(|rest| rest.split_whitespace().next().unwrap().to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn every_outcome_is_reported_and_a_real_failure_fails_the_run() {
    let state = scratch("outcomes");
    let (junit, html, history) = (
        state.join("out/junit.xml"),
        state.join("out/report.html"),
        state.join("out/history.json"),
    );
    let (output, stdout) = run(
        &state,
        &[
            "--retries",
            "1",
            "--reporter",
            &format!("junit:{}", junit.display()),
            "--reporter",
            &format!("html:{}", html.display()),
            "--flaky-history",
            history.to_str().unwrap(),
            "--",
            "tests::",
        ],
    );
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("Running 4 tests from 1 binary"), "{stdout}");
    assert_eq!(labelled(&stdout, "PASS"), ["run-fixture::tests::passes"]);
    assert_eq!(labelled(&stdout, "FLAKY"), ["run-fixture::tests::flaky"]);
    assert_eq!(
        labelled(&stdout, "FAIL"),
        [
            "run-fixture::tests::fails",
            "run-fixture::tests::flaky",
            "run-fixture::tests::prints_then_fails"
        ]
    );
    assert!(stdout.contains("assertion `left == right` failed: arithmetic is broken"));
    assert!(stdout.contains("diagnostic: the widget count was 7"));
    assert!(stdout.contains("Results: 2 passed, 2 failed, 1 flaky, 1 skipped, 5 total"));

    let junit = std::fs::read_to_string(junit).unwrap();
    assert!(junit.contains(r#"<flakyFailure message="passed after 2 attempts"/>"#));
    assert!(
        junit.contains(r#"<testcase name="tests::ignored" classname="run-fixture" time="0.000">"#)
    );
    assert!(
        std::fs::read_to_string(html)
            .unwrap()
            .contains("widgets went missing")
    );
    let history: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(history).unwrap()).unwrap();
    assert_eq!(history["run-fixture::tests::flaky"]["flakyPasses"], 1);
    assert_eq!(history["run-fixture::tests::fails"]["failures"], 1);
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn quarantine_and_skip_let_a_run_pass_and_flaky_tests_can_fail_it() {
    let state = scratch("quarantine");
    let list = state.join("quarantine.txt");
    std::fs::write(&list, "# known\nrun-fixture::tests::fails\n").unwrap();
    let quarantine = ["--quarantine", list.to_str().unwrap()];

    let (output, stdout) = run(
        &state,
        &[
            quarantine[0],
            quarantine[1],
            "--",
            "tests::",
            "--skip",
            "prints",
            "--skip",
            "flaky",
        ],
    );
    assert!(output.status.success(), "{stdout}");
    assert_eq!(labelled(&stdout, "QUAR"), ["run-fixture::tests::fails"]);

    let (output, stdout) = run(
        &state,
        &["--retries", "1", "--fail-on-flaky", "--", "flaky"],
    );
    assert!(!output.status.success(), "{stdout}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--fail-on-flaky"));
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn shards_are_disjoint_and_cover_the_suite() {
    let state = scratch("shards");
    let ran = |args: &[&str]| {
        let (_, stdout) = run(&state, args);
        let mut names = labelled(&stdout, "PASS");
        names.extend(labelled(&stdout, "FAIL"));
        names.sort();
        names
    };
    let all = ran(&["--", "--skip", "flaky"]);
    let first = ran(&["--shard", "1/2", "--", "--skip", "flaky"]);
    let second = ran(&["--shard", "2/2", "--", "--skip", "flaky"]);
    assert!(!first.is_empty() && !second.is_empty());
    assert!(first.iter().all(|t| !second.contains(t)));
    let mut both = [first, second].concat();
    both.sort();
    assert_eq!(both, all);
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn a_crash_is_pinned_on_the_test_that_died() {
    let state = scratch("crash");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/Cargo.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_agito"))
        .args([
            "run",
            "--manifest-path",
            fixture.to_str().unwrap(),
            "--",
            "crash::",
        ])
        .env("CARGO_TARGET_DIR", workspace().join("target/run-fixture"))
        .env("FIXTURE_CRASH", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success());
    assert_eq!(labelled(&stdout, "FAIL"), ["run-fixture::crash::aborts"]);
    assert_eq!(labelled(&stdout, "PASS"), ["run-fixture::crash::survives"]);
    assert!(stdout.contains("the test binary died while running this test"));
    std::fs::remove_dir_all(state).unwrap();
}

#[cfg(feature = "integration")]
#[test]
fn agito_tests_report_durations_messages_and_quarantine() {
    let state = scratch("browser");
    let (output, stdout) = run(&state, &["--features", "browser", "--", "browser::"]);
    assert!(!output.status.success(), "{stdout}");
    assert_eq!(labelled(&stdout, "PASS"), ["run-fixture::browser::loads"]);
    assert_eq!(
        labelled(&stdout, "FAIL"),
        ["run-fixture::browser::fails_with_an_error"]
    );
    assert_eq!(
        labelled(&stdout, "QUAR"),
        ["run-fixture::browser::quarantined"]
    );
    assert!(stdout.contains("quarantined: tracked in issue 12"));
    assert!(stdout.contains("the checkout button never appeared"));
    let loads = stdout
        .lines()
        .find(|l| l.contains("browser::loads"))
        .unwrap();
    assert!(loads.trim_end().ends_with("s)"), "a duration: {loads}");
    std::fs::remove_dir_all(state).unwrap();
}
