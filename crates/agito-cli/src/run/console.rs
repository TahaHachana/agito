//! What `agito run` prints as it goes, and its summary.

use std::io::IsTerminal;
use std::time::Duration;

use super::execute::{Outcome, Status};
use super::select::Shard;

/// Counts over a run's outcomes. Quarantined tests are counted apart, whatever their outcome,
/// since they do not decide the run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub passed: usize,
    pub failed: usize,
    /// Passed on a retry; also counted in `passed`.
    pub flaky: usize,
    pub quarantined: usize,
    pub skipped: usize,
}

impl Summary {
    pub fn of(outcomes: &[Outcome]) -> Summary {
        let mut summary = Summary::default();
        for o in outcomes {
            if o.quarantine.is_some() {
                summary.quarantined += 1;
                continue;
            }
            match o.status {
                Status::Passed => {
                    summary.passed += 1;
                    summary.flaky += usize::from(o.flaky);
                }
                Status::Failed => summary.failed += 1,
                Status::Skipped => summary.skipped += 1,
            }
        }
        summary
    }

    pub fn total(&self) -> usize {
        self.passed + self.failed + self.quarantined + self.skipped
    }
}

pub struct Console {
    color: bool,
}

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const GRAY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

impl Console {
    pub fn new() -> Console {
        Console {
            color: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, color: &str, text: &str) -> String {
        if self.color {
            format!("{color}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    pub fn start(&self, tests: usize, binaries: usize, shard: Option<Shard>) {
        let shard = shard
            .map(|s| format!(" (shard {}/{})", s.index, s.total))
            .unwrap_or_default();
        println!(
            "Running {tests} test{} from {binaries} binar{}{shard}",
            plural(tests),
            if binaries == 1 { "y" } else { "ies" }
        );
    }

    pub fn outcome(&self, outcome: &Outcome) {
        let (color, label) = label(outcome);
        let duration = outcome
            .duration
            .map(|d| self.paint(GRAY, &format!(" ({})", seconds(d))))
            .unwrap_or_default();
        println!(
            "  {} {}{duration}",
            self.paint(color, &format!("{label:<5}")),
            outcome.full_name
        );
        if let (Some(reason), Status::Failed) = (&outcome.quarantine, outcome.status) {
            println!(
                "        {}",
                self.paint(GRAY, &format!("quarantined: {reason}"))
            );
        }
    }

    pub fn retrying(&self, count: usize, attempt: u32, attempts: u32) {
        println!(
            "Retrying {count} failed test{} (attempt {attempt} of {attempts})",
            plural(count)
        );
    }

    /// The failures' output, then the counts.
    pub fn finish(&self, outcomes: &[Outcome], summary: &Summary, elapsed: Duration) {
        let failures: Vec<&Outcome> = outcomes
            .iter()
            .filter(|o| o.status == Status::Failed)
            .collect();
        if !failures.is_empty() {
            println!("\n{}", self.paint(BOLD, "Failures:"));
            for o in failures {
                let quarantined = if o.quarantine.is_some() {
                    " (quarantined)"
                } else {
                    ""
                };
                println!("\n---- {}{quarantined} ----", o.full_name);
                if let Some(message) = &o.message
                    && !o
                        .output
                        .as_ref()
                        .is_some_and(|out| out.contains(message.as_str()))
                {
                    println!("{message}");
                }
                if let Some(output) = &o.output {
                    print!("{}", ensure_newline(output));
                }
                if let Some(shot) = &o.screenshot {
                    println!("screenshot: {}", shot.display());
                }
            }
        }
        let flaky: Vec<&Outcome> = outcomes
            .iter()
            .filter(|o| o.flaky && o.quarantine.is_none())
            .collect();
        if !flaky.is_empty() {
            println!("\n{}", self.paint(BOLD, "Flaky (passed on a retry):"));
            for o in flaky {
                println!("  {} (after {} attempts)", o.full_name, o.attempts);
            }
        }

        let mut parts = vec![
            self.paint(GREEN, &format!("{} passed", summary.passed)),
            if summary.failed > 0 {
                self.paint(RED, &format!("{} failed", summary.failed))
            } else {
                "0 failed".to_owned()
            },
        ];
        if summary.flaky > 0 {
            parts.push(self.paint(YELLOW, &format!("{} flaky", summary.flaky)));
        }
        if summary.quarantined > 0 {
            parts.push(self.paint(GRAY, &format!("{} quarantined", summary.quarantined)));
        }
        if summary.skipped > 0 {
            parts.push(format!("{} skipped", summary.skipped));
        }
        println!(
            "\nResults: {}, {} total ({})",
            parts.join(", "),
            summary.total(),
            seconds(elapsed)
        );
    }
}

fn label(outcome: &Outcome) -> (&'static str, &'static str) {
    match (outcome.status, outcome.quarantine.is_some(), outcome.flaky) {
        (Status::Skipped, ..) => (GRAY, "SKIP"),
        (_, true, _) => (GRAY, "QUAR"),
        (Status::Passed, _, true) => (YELLOW, "FLAKY"),
        (Status::Passed, ..) => (GREEN, "PASS"),
        (Status::Failed, ..) => (RED, "FAIL"),
    }
}

pub fn seconds(d: Duration) -> String {
    format!("{:.2}s", d.as_secs_f64())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn ensure_newline(text: &str) -> String {
    if text.ends_with('\n') {
        text.to_owned()
    } else {
        format!("{text}\n")
    }
}
