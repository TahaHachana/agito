//! `agito shard merge`: combines the JUnit reports of a sharded run into one, and checks that
//! every shard is there.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};

use super::console::{Console, Summary};
use super::execute::{Outcome, Status};
use super::reports::{self, Reporter};
use super::select::Shard;

#[derive(Args, Debug)]
#[command(after_help = "Example:
  agito shard merge results/shard-*.xml --expect 4 --output junit:results/all.xml")]
pub struct MergeArgs {
    /// The shards' JUnit reports, from `agito run --shard … --reporter junit:…`. A pattern
    /// such as results/shard-*.xml is expanded here when the shell has not.
    #[arg(required = true, value_name = "FILES")]
    files: Vec<String>,

    /// Where to write the merged report: junit:<path> or html:<path>. Repeat for several;
    /// without one, only the summary is printed.
    #[arg(long = "output", value_name = "SPEC")]
    outputs: Vec<Reporter>,

    /// Fail unless the files are exactly shards 1 to N of N, each once.
    #[arg(long, value_name = "N")]
    expect: Option<usize>,
}

/// One shard's report.
#[derive(Debug)]
struct ShardReport {
    path: PathBuf,
    shard: Option<Shard>,
    outcomes: Vec<Outcome>,
    elapsed: Duration,
}

/// Merges the reports. Returns whether the merged run passed and every check held.
pub fn merge(args: MergeArgs) -> Result<bool> {
    let files = expand(&args.files)?;
    let reports: Vec<ShardReport> = files.iter().map(|f| parse_file(f)).collect::<Result<_>>()?;

    let mut problems = match args.expect {
        Some(expected) => check_shards(&reports, expected),
        None => Vec::new(),
    };

    let mut merged: BTreeMap<String, (Outcome, &Path)> = BTreeMap::new();
    for report in &reports {
        for outcome in &report.outcomes {
            match merged.get_mut(&outcome.full_name) {
                None => {
                    merged.insert(outcome.full_name.clone(), (outcome.clone(), &report.path));
                }
                Some((kept, first)) => {
                    problems.push(format!(
                        "{} is in both {} and {}; shards should not overlap",
                        outcome.full_name,
                        first.display(),
                        report.path.display()
                    ));
                    // Keep the worse result: a failure is not hidden by a pass elsewhere.
                    if outcome.status == Status::Failed {
                        *kept = outcome.clone();
                    }
                }
            }
        }
    }
    let outcomes: Vec<Outcome> = merged.into_values().map(|(o, _)| o).collect();
    let summary = Summary::of(&outcomes);
    let elapsed: Duration = reports.iter().map(|r| r.elapsed).sum();

    let shards: BTreeSet<String> = reports
        .iter()
        .filter_map(|r| r.shard.map(|s| format!("{}/{}", s.index, s.total)))
        .collect();
    println!(
        "Merged {} report{}{}",
        reports.len(),
        if reports.len() == 1 { "" } else { "s" },
        if shards.is_empty() {
            String::new()
        } else {
            format!(
                " (shards {})",
                shards.into_iter().collect::<Vec<_>>().join(", ")
            )
        }
    );
    // The time is the shards' added together: what the suite cost, not how long CI waited.
    Console::new().finish(&outcomes, &summary, elapsed);

    let run = reports::Run {
        outcomes: &outcomes,
        summary,
        elapsed,
        shard: None,
    };
    for output in &args.outputs {
        output.write(&run)?;
    }
    for problem in &problems {
        eprintln!("error: {problem}");
    }
    Ok(summary.failed == 0 && problems.is_empty())
}

/// What is wrong with the set of shards, against `expected` shards in all.
fn check_shards(reports: &[ShardReport], expected: usize) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen: HashMap<usize, &Path> = HashMap::new();
    for report in reports {
        let Some(shard) = report.shard else {
            problems.push(format!(
                "{} records no shard; write it with `agito run --shard`",
                report.path.display()
            ));
            continue;
        };
        if shard.total != expected {
            problems.push(format!(
                "{} is shard {}/{}, but --expect is {expected}",
                report.path.display(),
                shard.index,
                shard.total
            ));
        }
        if let Some(first) = seen.insert(shard.index, &report.path) {
            problems.push(format!(
                "shard {} is in both {} and {}",
                shard.index,
                first.display(),
                report.path.display()
            ));
        }
    }
    let missing: Vec<String> = (1..=expected)
        .filter(|i| !seen.contains_key(i))
        .map(|i| i.to_string())
        .collect();
    if !missing.is_empty() {
        problems.push(format!(
            "shard{} {} of {expected} {} missing",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", "),
            if missing.len() == 1 { "is" } else { "are" }
        ));
    }
    problems
}

/// The files named, with `*` and `?` in a file name expanded, in order and without repeats.
/// Shells on Windows leave patterns to the program.
fn expand(args: &[String]) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for arg in args {
        let path = Path::new(arg);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.contains(['*', '?']) {
            if !files.iter().any(|f| f == path) {
                files.push(path.to_path_buf());
            }
            continue;
        }
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut matched: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("could not read {}", dir.display()))?
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(|n| wildcard(name, n)))
            .map(|e| dir.join(e.file_name()))
            .collect();
        if matched.is_empty() {
            bail!("no files match {arg}");
        }
        matched.sort();
        for m in matched {
            if !files.contains(&m) {
                files.push(m);
            }
        }
    }
    Ok(files)
}

/// `*` matches any run of characters and `?` any one.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // matches[j]: pattern[..i] matches name[..j].
    let mut matches = vec![false; n.len() + 1];
    matches[0] = true;
    for &pc in &p {
        let mut next = vec![false; n.len() + 1];
        if pc == '*' {
            let mut any = false;
            for j in 0..=n.len() {
                any |= matches[j];
                next[j] = any;
            }
        } else {
            for j in 1..=n.len() {
                next[j] = matches[j - 1] && (pc == '?' || pc == n[j - 1]);
            }
        }
        matches = next;
    }
    matches[n.len()]
}

fn parse_file(path: &Path) -> Result<ShardReport> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let (shard, outcomes, elapsed) = parse(&text)
        .with_context(|| format!("{} is not a JUnit report agito can read", path.display()))?;
    Ok(ShardReport {
        path: path.to_path_buf(),
        shard,
        outcomes,
        elapsed,
    })
}

/// A `<testcase>` being read.
#[derive(Default)]
struct Case {
    binary: String,
    name: String,
    time: f64,
    properties: HashMap<String, String>,
    failure: Option<(String, String)>,
    skipped: bool,
    flaky: bool,
    attachments: Vec<PathBuf>,
}

impl Case {
    fn into_outcome(self) -> Outcome {
        let quarantine = self.properties.get(reports::QUARANTINE).cloned();
        let status = if self.skipped {
            Status::Skipped
        } else if self.failure.is_some()
            || self.properties.get(reports::OUTCOME).map(String::as_str) == Some("failed")
        {
            Status::Failed
        } else {
            Status::Passed
        };
        let (message, output) = match self.failure {
            Some((message, output)) => (Some(message), (!output.is_empty()).then_some(output)),
            None => (self.properties.get(reports::MESSAGE).cloned(), None),
        };
        Outcome {
            full_name: format!("{}::{}", self.binary, self.name),
            binary: self.binary,
            name: self.name,
            status,
            // The report writes 0 for a test that did not time itself.
            duration: (self.time > 0.0).then(|| Duration::from_secs_f64(self.time)),
            attempts: self
                .properties
                .get(reports::ATTEMPTS)
                .and_then(|a| a.parse().ok())
                .unwrap_or(1),
            flaky: self.flaky,
            quarantine,
            message,
            output,
            accessibility: self
                .properties
                .get(reports::ACCESSIBILITY)
                .and_then(|json| serde_json::from_str(json).ok())
                .unwrap_or_default(),
            screenshot: self
                .attachments
                .iter()
                .find(|a| !a.to_string_lossy().ends_with(".zip"))
                .cloned(),
            trace: self
                .attachments
                .iter()
                .find(|a| a.to_string_lossy().ends_with(".zip"))
                .cloned(),
        }
    }
}

type Parsed = (Option<Shard>, Vec<Outcome>, Duration);

/// Reads a JUnit report: the shard, every test case, and the run's time.
fn parse(text: &str) -> Result<Parsed> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<String> = Vec::new();
    let mut suite_properties: HashMap<String, String> = HashMap::new();
    let mut outcomes = Vec::new();
    let mut elapsed = Duration::ZERO;
    let mut case: Option<Case> = None;
    let mut text_buffer = String::new();

    loop {
        let event = reader.read_event()?;
        let (element, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = element {
            let name = e.local_name().as_ref().to_owned();
            let attr = |key: &str| attribute(e, key);
            match name.as_str() {
                "testsuites" => {
                    elapsed = Duration::from_secs_f64(
                        attr("time")?.and_then(|t| t.parse().ok()).unwrap_or(0.0),
                    );
                }
                "testcase" => {
                    case = Some(Case {
                        binary: attr("classname")?.unwrap_or_default(),
                        name: attr("name")?.unwrap_or_default(),
                        time: attr("time")?.and_then(|t| t.parse().ok()).unwrap_or(0.0),
                        ..Case::default()
                    });
                }
                "property" => {
                    let (Some(key), Some(value)) = (attr("name")?, attr("value")?) else {
                        bail!("a <property> without a name or value");
                    };
                    match (&mut case, stack.last().map(String::as_str)) {
                        (Some(c), Some("properties")) => {
                            c.properties.insert(key, value);
                        }
                        (None, Some("properties")) => {
                            suite_properties.insert(key, value);
                        }
                        _ => {}
                    }
                }
                "failure" | "error" => {
                    if let Some(c) = &mut case {
                        c.failure = Some((attr("message")?.unwrap_or_default(), String::new()));
                    }
                }
                "skipped" => {
                    if let Some(c) = &mut case {
                        c.skipped = true;
                    }
                }
                "flakyFailure" => {
                    if let Some(c) = &mut case {
                        c.flaky = true;
                    }
                }
                _ => {}
            }
            text_buffer.clear();
            if !empty {
                stack.push(name);
            } else if name == "testcase"
                && let Some(c) = case.take()
            {
                outcomes.push(c.into_outcome());
            }
            continue;
        }
        match event {
            Event::Text(t) => text_buffer.push_str(&t.xml10_content()),
            Event::CData(t) => text_buffer.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => {
                if let Some(c) = r.resolve_char_ref()? {
                    text_buffer.push(c);
                } else if let Some(entity) = resolve_predefined_entity(&r.xml10_content()) {
                    text_buffer.push_str(entity);
                }
            }
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                match (name.as_str(), &mut case) {
                    ("failure" | "error", Some(c)) => {
                        if let Some((_, output)) = &mut c.failure {
                            *output = std::mem::take(&mut text_buffer);
                        }
                    }
                    ("system-err", Some(c)) => {
                        let text = std::mem::take(&mut text_buffer);
                        c.attachments.extend(text.lines().filter_map(|line| {
                            let path = line.trim().strip_prefix("[[ATTACHMENT|")?;
                            Some(PathBuf::from(path.strip_suffix("]]")?))
                        }));
                    }
                    ("testcase", _) => {
                        if let Some(c) = case.take() {
                            outcomes.push(c.into_outcome());
                        }
                    }
                    _ => {}
                }
                text_buffer.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let shard = match (
        suite_properties.get(reports::SHARD_INDEX),
        suite_properties.get(reports::SHARD_TOTAL),
    ) {
        (Some(index), Some(total)) => Some(
            format!("{index}/{total}")
                .parse::<Shard>()
                .map_err(anyhow::Error::msg)?,
        ),
        _ => None,
    };
    Ok((shard, outcomes, elapsed))
}

fn attribute(element: &BytesStart, key: &str) -> Result<Option<String>> {
    for attr in element.attributes() {
        let attr = attr?;
        if attr.key.local_name().as_ref() == key {
            return Ok(Some(
                attr.normalized_value(XmlVersion::Implicit1_0)?.into_owned(),
            ));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(binary: &str, name: &str, status: Status) -> Outcome {
        Outcome {
            full_name: format!("{binary}::{name}"),
            binary: binary.into(),
            name: name.into(),
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

    fn junit(outcomes: &[Outcome], shard: Option<Shard>) -> String {
        reports::junit(&reports::Run {
            outcomes,
            summary: Summary::of(outcomes),
            elapsed: Duration::from_millis(2500),
            shard,
        })
    }

    #[test]
    fn a_report_reads_back_as_it_was_written() {
        let mut failed = outcome("pkg::ui", "checkout <fails>", Status::Failed);
        failed.message = Some("expected \"Paid\"\n  got: none".into());
        failed.output = Some("thread panicked\nexpected \"Paid\" & got <none>\n".into());
        failed.screenshot = Some(PathBuf::from("/tmp/shots/checkout.png"));
        failed.trace = Some(PathBuf::from("/tmp/shots/checkout.trace.zip"));
        failed.duration = Some(Duration::from_millis(1250));
        let mut flaky = outcome("pkg", "tests::flaky", Status::Passed);
        flaky.flaky = true;
        flaky.attempts = 3;
        let mut quarantined = outcome("pkg::ui", "known", Status::Failed);
        quarantined.quarantine = Some("issue 12".into());
        quarantined.message = Some("still broken".into());
        let mut quarantined_pass = outcome("pkg::ui", "recovered", Status::Passed);
        quarantined_pass.quarantine = Some("issue 13".into());
        quarantined_pass.accessibility = vec![
            agito::accessibility::Violation::new(
                "a11y-empty-button",
                agito::accessibility::Severity::Error,
                "Button has no \"name\" <here>.",
            )
            .at_selector("#close"),
        ];
        let written = vec![
            outcome("pkg", "tests::passes", Status::Passed),
            outcome("pkg", "tests::ignored", Status::Skipped),
            failed,
            flaky,
            quarantined,
            quarantined_pass,
        ];

        let (shard, read, elapsed) =
            parse(&junit(&written, Some(Shard { index: 3, total: 4 }))).unwrap();
        assert_eq!(shard, Some(Shard { index: 3, total: 4 }));
        assert_eq!(elapsed, Duration::from_millis(2500));
        let by_name: HashMap<&str, &Outcome> =
            read.iter().map(|o| (o.full_name.as_str(), o)).collect();
        assert_eq!(read.len(), written.len());
        for w in &written {
            let r = by_name[w.full_name.as_str()];
            assert_eq!(
                (
                    r.status,
                    r.flaky,
                    r.attempts,
                    &r.quarantine,
                    r.duration,
                    &r.screenshot,
                    &r.trace
                ),
                (
                    w.status,
                    w.flaky,
                    w.attempts,
                    &w.quarantine,
                    w.duration,
                    &w.screenshot,
                    &w.trace
                ),
                "{}",
                w.full_name
            );
        }
        let f = by_name["pkg::ui::checkout <fails>"];
        assert_eq!(f.message.as_deref(), Some("expected \"Paid\"\n  got: none"));
        assert_eq!(
            f.output.as_deref(),
            Some("thread panicked\nexpected \"Paid\" & got <none>\n")
        );
        assert_eq!(
            by_name["pkg::ui::known"].message.as_deref(),
            Some("still broken")
        );
        assert_eq!(
            by_name["pkg::ui::recovered"].accessibility, written[5].accessibility,
            "violations survive a merge"
        );
    }

    #[test]
    fn an_empty_shard_still_records_its_number() {
        let (shard, outcomes, _) = parse(&junit(&[], Some(Shard { index: 2, total: 2 }))).unwrap();
        assert_eq!(shard, Some(Shard { index: 2, total: 2 }));
        assert!(outcomes.is_empty());
    }

    #[test]
    fn shard_checks_name_what_is_missing_duplicated_or_unmarked() {
        let report = |path: &str, shard: Option<(usize, usize)>| ShardReport {
            path: path.into(),
            shard: shard.map(|(index, total)| Shard { index, total }),
            outcomes: Vec::new(),
            elapsed: Duration::ZERO,
        };
        let complete = [
            report("a", Some((1, 3))),
            report("b", Some((2, 3))),
            report("c", Some((3, 3))),
        ];
        assert!(check_shards(&complete, 3).is_empty());

        let problems = check_shards(
            &[
                report("a", Some((1, 4))),
                report("b", Some((1, 4))),
                report("c", None),
            ],
            4,
        );
        assert_eq!(
            problems,
            [
                "shard 1 is in both a and b",
                "c records no shard; write it with `agito run --shard`",
                "shards 2, 3, 4 of 4 are missing",
            ]
        );
        let problems = check_shards(&[report("a", Some((1, 2)))], 1);
        assert_eq!(problems, ["a is shard 1/2, but --expect is 1"]);
    }

    #[test]
    fn wildcards_match_file_names() {
        assert!(wildcard("shard-*.xml", "shard-1.xml"));
        assert!(wildcard("shard-?.xml", "shard-2.xml"));
        assert!(!wildcard("shard-?.xml", "shard-10.xml"));
        assert!(wildcard("*", "anything"));
        assert!(!wildcard("shard-*.xml", "shard-1.json"));
    }

    #[test]
    fn patterns_expand_in_order() {
        let dir = std::env::temp_dir().join(format!("agito-merge-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["shard-2.xml", "shard-1.xml", "other.txt"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let pattern = dir.join("shard-*.xml").to_string_lossy().into_owned();
        let files = expand(&[
            pattern.clone(),
            dir.join("shard-1.xml").to_string_lossy().into(),
        ])
        .unwrap();
        assert_eq!(files, [dir.join("shard-1.xml"), dir.join("shard-2.xml")]);
        assert!(expand(&[dir.join("none-*.xml").to_string_lossy().into()]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
