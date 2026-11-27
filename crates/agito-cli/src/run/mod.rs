//! `agito run`: builds the test binaries with cargo, runs them itself, and reports.
//!
//! Running the binaries rather than `cargo test` is what makes the rest possible: each result
//! is tied to its binary, a failed test can be re-run alone, and a shard is an exact list of
//! tests. cargo-nextest works the same way.

mod console;
mod discover;
mod execute;
mod reports;
mod select;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use anyhow::{Result, bail};
use clap::Args;

use console::{Console, Summary};
use discover::TestCase;
use execute::{Outcome, Settings, Status};
use reports::Reporter;
use select::{Filters, Shard};

#[derive(Args, Debug)]
#[command(after_help = "Examples:
  agito run
  agito run -p my-app --features e2e -- checkout
  agito run --shard 2/4 --reporter junit:results/shard-2.xml
  agito run --retries 2 --fail-on-flaky --flaky-history target/flaky.json")]
pub struct RunArgs {
    /// Run one shard of the suite, such as 2/4. Shards are disjoint, and together they run
    /// everything.
    #[arg(long, value_name = "INDEX/TOTAL")]
    shard: Option<Shard>,

    /// Tests run at once in each binary: a number, or auto for one per CPU (the default).
    #[arg(long, value_name = "N|auto")]
    workers: Option<String>,

    /// A report to write: junit:<path> or html:<path>. Repeat for several. The console report
    /// is always shown.
    #[arg(long = "reporter", value_name = "SPEC")]
    reporters: Vec<Reporter>,

    /// Re-run a failed test up to N more times. One that then passes is reported as flaky.
    #[arg(long, default_value_t = 0, value_name = "N")]
    retries: u32,

    /// Fail the run when a test is flaky.
    #[arg(long)]
    fail_on_flaky: bool,

    /// A file of full test names, one per line, whose failures do not fail the run.
    #[arg(long, value_name = "FILE")]
    quarantine: Option<PathBuf>,

    /// A JSON file that accumulates each test's runs, failures and flaky passes.
    #[arg(long, value_name = "FILE")]
    flaky_history: Option<PathBuf>,

    /// Arguments for `cargo test --no-run`, such as -p, --features or --release.
    #[arg(allow_hyphen_values = true, value_name = "CARGO_ARGS")]
    cargo_args: Vec<String>,

    /// After `--`: test name patterns, --exact, --skip <pattern>, --ignored, --include-ignored.
    #[arg(last = true, allow_hyphen_values = true, value_name = "TEST_ARGS")]
    test_args: Vec<String>,
}

impl RunArgs {
    /// cargo's arguments and the tests'. clap keeps a `--` that follows cargo arguments among
    /// them, so the split is made here.
    fn split(&self) -> (Vec<String>, Vec<String>) {
        let mut cargo = self.cargo_args.clone();
        let mut tests = Vec::new();
        if let Some(at) = cargo.iter().position(|a| a == "--") {
            tests = cargo.split_off(at + 1);
            cargo.pop();
        }
        tests.extend(self.test_args.iter().cloned());
        (cargo, tests)
    }
}

/// Runs the suite. Returns whether it passed.
pub fn run(args: RunArgs) -> Result<bool> {
    let started = Instant::now();
    let (cargo_args, test_args) = args.split();
    let filters = Filters::parse(&test_args)?;
    let test_threads = match args.workers.as_deref() {
        None | Some("auto") => None,
        Some(n) => match n.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => bail!("--workers takes a positive number or auto, not '{n}'"),
        },
    };
    let quarantine_list = match &args.quarantine {
        Some(path) => select::read_quarantine(path)?,
        None => HashSet::new(),
    };

    let binaries: Vec<Arc<discover::Binary>> = discover::build(&cargo_args)?
        .into_iter()
        .map(Arc::new)
        .collect();
    let mut selected = Vec::new();
    for binary in &binaries {
        selected.extend(
            discover::list(binary)?
                .into_iter()
                .filter(|t| filters.selects(t)),
        );
    }
    if let Some(shard) = args.shard {
        selected = shard.select(selected);
    }
    let (to_run, skipped): (Vec<TestCase>, Vec<TestCase>) =
        selected.into_iter().partition(|t| filters.runs(t));

    let console = Console::new();
    let binary_count = to_run
        .iter()
        .map(|t| t.binary.id.as_str())
        .collect::<HashSet<_>>()
        .len();
    console.start(to_run.len(), binary_count, args.shard);

    let results_root = std::env::temp_dir().join(format!("agito-run-{}", std::process::id()));
    let settings = Settings {
        test_threads,
        include_ignored: filters.include_ignored || filters.ignored_only,
        results_root: results_root.clone(),
    };
    let mark = |outcome: &mut Outcome| {
        if outcome.quarantine.is_none() && quarantine_list.contains(&outcome.full_name) {
            outcome.quarantine = Some("listed in the quarantine file".into());
        }
        console.outcome(outcome);
    };

    let mut outcomes: BTreeMap<String, Outcome> = BTreeMap::new();
    for binary in &binaries {
        let tests: Vec<&TestCase> = to_run.iter().filter(|t| t.binary.id == binary.id).collect();
        if tests.is_empty() {
            continue;
        }
        // Without names, a binary runs what libtest runs by default; when that is exactly the
        // selection, the list need not be passed.
        let whole = args.shard.is_none()
            && filters.patterns.is_empty()
            && filters.skip.is_empty()
            && !filters.ignored_only;
        for outcome in execute::run(binary, &tests, whole, &settings, &mut |o| mark(o))? {
            outcomes.insert(outcome.full_name.clone(), outcome);
        }
    }

    // Each round re-runs what still fails, a binary at a time.
    for attempt in 2..=args.retries + 1 {
        let failing: Vec<&TestCase> = to_run
            .iter()
            .filter(|t| {
                outcomes
                    .get(&t.full_name())
                    .is_some_and(Outcome::gating_failure)
            })
            .collect();
        if failing.is_empty() {
            break;
        }
        console.retrying(failing.len(), attempt, args.retries + 1);
        for binary in &binaries {
            let tests: Vec<&TestCase> = failing
                .iter()
                .copied()
                .filter(|t| t.binary.id == binary.id)
                .collect();
            if tests.is_empty() {
                continue;
            }
            let mut on_retry = |o: &mut Outcome| {
                o.attempts = attempt;
                o.flaky = o.status == Status::Passed;
                mark(o);
            };
            for outcome in execute::run(binary, &tests, false, &settings, &mut on_retry)? {
                outcomes.insert(outcome.full_name.clone(), outcome);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&results_root);

    let mut all: Vec<Outcome> = outcomes.into_values().collect();
    all.extend(skipped.iter().map(Outcome::skipped));
    let summary = Summary::of(&all);
    let elapsed = started.elapsed();
    console.finish(&all, &summary, elapsed);

    let report = reports::Run {
        outcomes: &all,
        summary,
        elapsed,
        shard: args.shard,
    };
    for reporter in &args.reporters {
        reporter.write(&report)?;
    }
    if let Some(path) = &args.flaky_history {
        reports::update_history(path, &all, SystemTime::now())?;
    }

    let flaky_fails = args.fail_on_flaky && summary.flaky > 0;
    if flaky_fails {
        eprintln!(
            "error: {} flaky test(s), and --fail-on-flaky is set",
            summary.flaky
        );
    }
    Ok(summary.failed == 0 && !flaky_fails)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        run: RunArgs,
    }

    fn parse(args: &[&str]) -> RunArgs {
        Cli::try_parse_from(std::iter::once("run").chain(args.iter().copied()))
            .unwrap()
            .run
    }

    #[test]
    fn cargo_args_and_test_args_split_at_the_double_dash() {
        let args = parse(&[
            "--shard",
            "1/2",
            "-p",
            "agito",
            "--features",
            "integration",
            "--",
            "popups",
            "--skip",
            "slow",
        ]);
        assert_eq!(args.shard, Some(Shard { index: 1, total: 2 }));
        let (cargo, tests) = args.split();
        assert_eq!(cargo, ["-p", "agito", "--features", "integration"]);
        assert_eq!(tests, ["popups", "--skip", "slow"]);

        let (cargo, tests) = parse(&[]).split();
        assert!(cargo.is_empty() && tests.is_empty());
        let (cargo, tests) = parse(&["--", "--exact", "x"]).split();
        assert!(cargo.is_empty(), "{cargo:?}");
        assert_eq!(tests, ["--exact", "x"]);
        let (cargo, tests) = parse(&["--workspace"]).split();
        assert_eq!(cargo, ["--workspace"]);
        assert!(tests.is_empty());
    }
}
