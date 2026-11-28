//! Reports written at the end of a run: JUnit XML, a standalone HTML page, and flaky history.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::console::{Summary, seconds};
use super::execute::{Outcome, Status};
use super::select::Shard;

/// `--reporter`: `console`, `junit:<path>` or `html:<path>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reporter {
    /// Always on; accepted so Motus command lines keep working.
    Console,
    Junit(PathBuf),
    Html(PathBuf),
}

impl FromStr for Reporter {
    type Err = String;

    fn from_str(spec: &str) -> std::result::Result<Self, String> {
        let (kind, path) = spec.split_once(':').unwrap_or((spec, ""));
        let path = || {
            if path.is_empty() {
                Err(format!(
                    "'{spec}' needs a path, such as {kind}:results.{kind}"
                ))
            } else {
                Ok(PathBuf::from(path))
            }
        };
        match kind {
            "console" => Ok(Reporter::Console),
            "junit" => Ok(Reporter::Junit(path()?)),
            "html" => Ok(Reporter::Html(path()?)),
            _ => Err(format!(
                "unknown reporter '{kind}': expected console, junit:<path> or html:<path>"
            )),
        }
    }
}

/// What every report needs.
pub struct Run<'a> {
    pub outcomes: &'a [Outcome],
    pub summary: Summary,
    pub elapsed: Duration,
    pub shard: Option<Shard>,
}

impl Reporter {
    pub fn write(&self, run: &Run) -> Result<()> {
        let (path, text) = match self {
            Reporter::Console => return Ok(()),
            Reporter::Junit(path) => (path, junit(run)),
            Reporter::Html(path) => (path, html(run)),
        };
        write_file(path, &text)?;
        println!("Report written to {}", path.display());
        Ok(())
    }
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

/// Escaped for an attribute value. Parsers turn a raw newline or tab in an attribute into a
/// space, so they are written as character references, which survive.
fn xml_attr(text: &str) -> String {
    xml(text)
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot carry other control characters at all, escaped or not.
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn secs(d: Option<Duration>) -> String {
    format!("{:.3}", d.unwrap_or_default().as_secs_f64())
}

/// One `<testsuite>` per binary. A quarantined failure is not a `<failure>`: it is noted in
/// `<system-out>`, so CI shows it without failing. A flaky pass carries `<flakyFailure>`, the
/// Surefire convention.
///
/// What `agito shard merge` needs to read back is also kept as properties: the shard on each
/// suite, and a test's quarantine, outcome and attempts on the test. A shard that ran nothing
/// still writes an empty suite, so its shard is on record.
pub fn junit(run: &Run) -> String {
    let mut suites: BTreeMap<&str, Vec<&Outcome>> = BTreeMap::new();
    for o in run.outcomes {
        suites.entry(o.binary.as_str()).or_default().push(o);
    }
    if suites.is_empty() && run.shard.is_some() {
        suites.insert(EMPTY_SHARD_SUITE, Vec::new());
    }
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let s = &run.summary;
    let _ = writeln!(
        out,
        "<testsuites name=\"agito\" tests=\"{}\" failures=\"{}\" skipped=\"{}\" time=\"{:.3}\">",
        s.total(),
        s.failed,
        s.skipped,
        run.elapsed.as_secs_f64()
    );
    for (binary, outcomes) in suites {
        let suite = Summary::of(&outcomes.iter().map(|o| (*o).clone()).collect::<Vec<_>>());
        let time: Duration = outcomes.iter().filter_map(|o| o.duration).sum();
        let _ = writeln!(
            out,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" skipped=\"{}\" time=\"{:.3}\">",
            xml(binary),
            suite.total(),
            suite.failed,
            suite.skipped,
            time.as_secs_f64()
        );
        if let Some(shard) = run.shard {
            let _ = writeln!(
                out,
                "    <properties>\n      <property name=\"{SHARD_INDEX}\" value=\"{}\"/>\n      <property name=\"{SHARD_TOTAL}\" value=\"{}\"/>\n    </properties>",
                shard.index, shard.total
            );
        }
        for o in outcomes {
            let _ = write!(
                out,
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{}\"",
                xml(&o.name),
                xml(binary),
                secs(o.duration)
            );
            let mut body = String::new();
            let message = o.message.as_deref().unwrap_or("failed");
            let mut properties: Vec<(&str, String)> = Vec::new();
            if let Some(reason) = &o.quarantine {
                properties.push((QUARANTINE, reason.clone()));
                let outcome = if o.status == Status::Failed {
                    "failed"
                } else {
                    "passed"
                };
                properties.push((OUTCOME, outcome.into()));
                if o.status == Status::Failed {
                    properties.push((MESSAGE, message.into()));
                }
            }
            if o.attempts > 1 {
                properties.push((ATTEMPTS, o.attempts.to_string()));
            }
            if !o.accessibility.is_empty() {
                let json = serde_json::to_string(&o.accessibility).expect("violations serialize");
                properties.push((ACCESSIBILITY, json));
            }
            if !properties.is_empty() {
                body.push_str("      <properties>\n");
                for (name, value) in properties {
                    let _ = writeln!(
                        body,
                        "        <property name=\"{name}\" value=\"{}\"/>",
                        xml_attr(&value)
                    );
                }
                body.push_str("      </properties>\n");
            }
            // Notes for <system-out>, of which a test case has at most one.
            let mut notes: Vec<String> = Vec::new();
            match (o.status, &o.quarantine) {
                (Status::Skipped, _) => body.push_str("      <skipped/>\n"),
                (status, Some(reason)) => {
                    let what = if status == Status::Passed {
                        "passed".to_owned()
                    } else {
                        format!("failed: {message}")
                    };
                    notes.push(format!("QUARANTINED ({reason}); {what}"));
                }
                (Status::Failed, None) => {
                    let _ = writeln!(
                        body,
                        "      <failure message=\"{}\">{}</failure>",
                        xml_attr(message),
                        xml(o.output.as_deref().unwrap_or(message))
                    );
                }
                (Status::Passed, None) if o.flaky => {
                    let _ = writeln!(
                        body,
                        "      <flakyFailure message=\"passed after {} attempts\"/>",
                        o.attempts
                    );
                }
                (Status::Passed, None) => {}
            }
            // Accessibility findings are notes, not failures: in enforce mode the harness has
            // already failed the test on them, and in warn mode they must not fail it.
            notes.extend(o.accessibility.iter().map(|v| format!("A11Y {v}")));
            if !notes.is_empty() {
                let _ = writeln!(
                    body,
                    "      <system-out>{}</system-out>",
                    xml(&notes.join("\n"))
                );
            }
            if let Some(shot) = &o.screenshot {
                // Jenkins' JUnit attachments convention.
                let _ = writeln!(
                    body,
                    "      <system-err>[[ATTACHMENT|{}]]</system-err>",
                    xml(&shot.display().to_string())
                );
            }
            if body.is_empty() {
                out.push_str("/>\n");
            } else {
                let _ = write!(out, ">\n{body}    </testcase>\n");
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

/// The suite an empty shard writes to record its shard.
pub const EMPTY_SHARD_SUITE: &str = "agito";
pub const SHARD_INDEX: &str = "agito.shard.index";
pub const SHARD_TOTAL: &str = "agito.shard.total";
pub const QUARANTINE: &str = "agito.quarantine";
/// How a quarantined test went, since its `<testcase>` shows no `<failure>`.
pub const OUTCOME: &str = "agito.outcome";
pub const MESSAGE: &str = "agito.message";
pub const ATTEMPTS: &str = "agito.attempts";
/// A test's accessibility violations, as JSON.
pub const ACCESSIBILITY: &str = "agito.accessibility";

/// A self-contained page: the counts, then every test, failures first, with their output and
/// screenshot inline.
pub fn html(run: &Run) -> String {
    let s = &run.summary;
    let mut rows: Vec<&Outcome> = run.outcomes.iter().collect();
    let rank = |o: &Outcome| match (o.status, o.quarantine.is_some(), o.flaky) {
        (Status::Failed, false, _) => 0,
        (Status::Passed, false, true) => 1,
        (_, true, _) => 2,
        (Status::Passed, ..) => 3,
        (Status::Skipped, ..) => 4,
    };
    rows.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.full_name.cmp(&b.full_name)));

    let mut out = String::from(HTML_HEAD);
    let shard = run
        .shard
        .map(|s| format!(" · shard {}/{}", s.index, s.total))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "<h1>Test report</h1><p class=\"meta\">{} tests in {}{shard}</p>",
        s.total(),
        seconds(run.elapsed)
    );
    out.push_str("<div class=\"stats\">");
    for (class, count, label) in [
        ("passed", s.passed, "passed"),
        ("failed", s.failed, "failed"),
        ("flaky", s.flaky, "flaky"),
        ("quarantined", s.quarantined, "quarantined"),
        ("skipped", s.skipped, "skipped"),
    ] {
        if count > 0 || matches!(class, "passed" | "failed") {
            let _ = write!(
                out,
                "<div class=\"stat {class}\"><b>{count}</b><span>{label}</span></div>"
            );
        }
    }
    out.push_str("</div>\n<ul>\n");
    for o in rows {
        let (class, label) = match (o.status, o.quarantine.is_some(), o.flaky) {
            (Status::Skipped, ..) => ("skipped", "skip"),
            (_, true, _) => ("quarantined", "quarantined"),
            (Status::Passed, _, true) => ("flaky", "flaky"),
            (Status::Passed, ..) => ("passed", "pass"),
            (Status::Failed, ..) => ("failed", "fail"),
        };
        let duration = o.duration.map(seconds).unwrap_or_default();
        let head = format!(
            "<span class=\"badge {class}\">{label}</span><span class=\"name\">{}</span><span class=\"time\">{duration}</span>",
            xml(&o.full_name)
        );
        let mut detail = String::new();
        if let Some(reason) = &o.quarantine {
            let _ = write!(detail, "<p class=\"note\">Quarantined: {}</p>", xml(reason));
        }
        if !o.accessibility.is_empty() {
            detail.push_str("<p class=\"note\">Accessibility:</p><ul class=\"a11y\">");
            for v in &o.accessibility {
                let _ = write!(detail, "<li>{}</li>", xml(&v.to_string()));
            }
            detail.push_str("</ul>");
        }
        if o.flaky {
            let _ = write!(
                detail,
                "<p class=\"note\">Passed after {} attempts.</p>",
                o.attempts
            );
        }
        if o.status == Status::Failed {
            if let Some(output) = o.output.as_ref().or(o.message.as_ref()) {
                let _ = write!(detail, "<pre>{}</pre>", xml(output));
            }
            if let Some(shot) = &o.screenshot
                && let Ok(png) = std::fs::read(shot)
            {
                let _ = write!(
                    detail,
                    "<img alt=\"Screenshot when it failed\" src=\"data:image/png;base64,{}\">",
                    base64::engine::general_purpose::STANDARD.encode(png)
                );
            }
        }
        if detail.is_empty() {
            let _ = writeln!(out, "<li><div class=\"row\">{head}</div></li>");
        } else {
            let open = if o.gating_failure() { " open" } else { "" };
            let _ = writeln!(
                out,
                "<li><details{open}><summary class=\"row\">{head}</summary><div class=\"detail\">{detail}</div></details></li>"
            );
        }
    }
    out.push_str("</ul>\n</main></body></html>\n");
    out
}

const HTML_HEAD: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>agito test report</title>
<style>
:root { color-scheme: light dark; --bg:#fff; --fg:#1c1c1e; --muted:#6b6b70; --line:#e4e4e7; --panel:#f7f7f8;
  --pass:#1a7f37; --fail:#cf222e; --flaky:#9a6700; --quar:#6e40c9; --skip:#6b6b70; }
@media (prefers-color-scheme: dark) { :root { --bg:#111113; --fg:#ececf0; --muted:#9a9aa3; --line:#2a2a2e; --panel:#1a1a1d;
  --pass:#3fb950; --fail:#f85149; --flaky:#d29922; --quar:#a371f7; --skip:#9a9aa3; } }
body { margin:0; background:var(--bg); color:var(--fg); font:14px/1.5 system-ui, sans-serif; }
main { max-width:960px; margin:0 auto; padding:32px 20px; }
h1 { font-size:22px; margin:0 0 4px; } .meta { color:var(--muted); margin:0 0 20px; }
.stats { display:flex; gap:12px; flex-wrap:wrap; margin-bottom:24px; }
.stat { border:1px solid var(--line); border-radius:8px; padding:10px 16px; min-width:90px; }
.stat b { display:block; font-size:22px; font-variant-numeric:tabular-nums; } .stat span { color:var(--muted); }
.stat.passed b { color:var(--pass); } .stat.failed b { color:var(--fail); } .stat.flaky b { color:var(--flaky); }
.stat.quarantined b { color:var(--quar); } .stat.skipped b { color:var(--skip); }
ul { list-style:none; padding:0; margin:0; border:1px solid var(--line); border-radius:8px; overflow:hidden; }
li + li { border-top:1px solid var(--line); }
.row { display:flex; gap:12px; align-items:baseline; padding:8px 14px; }
summary.row { cursor:pointer; } summary.row::marker { color:var(--muted); }
.name { flex:1; font-family:ui-monospace, monospace; font-size:13px; overflow-wrap:anywhere; }
.time { color:var(--muted); font-variant-numeric:tabular-nums; }
.badge { font-size:11px; font-weight:600; text-transform:uppercase; letter-spacing:.04em; width:92px; flex:none; }
.badge.passed { color:var(--pass); } .badge.failed { color:var(--fail); } .badge.flaky { color:var(--flaky); }
.badge.quarantined { color:var(--quar); } .badge.skipped { color:var(--skip); }
.detail { padding:0 14px 14px; } .note { color:var(--muted); margin:4px 0; }
.a11y { list-style:disc; border:0; padding-left:20px; margin:4px 0; font-size:13px; } .a11y li + li { border:0; }
pre { background:var(--panel); border:1px solid var(--line); border-radius:6px; padding:12px; overflow:auto; font-size:12px; margin:8px 0; }
img { max-width:100%; border:1px solid var(--line); border-radius:6px; margin-top:8px; }
</style></head><body><main>
"#;

/// One test's history across runs, in Motus's format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlakeRecord {
    pub runs: u64,
    pub failures: u64,
    /// Runs that passed only on a retry.
    pub flaky_passes: u64,
    pub last_seen_utc: String,
}

/// Adds this run to the history in `path`. A history that cannot be read is started afresh,
/// with a warning: it must not fail the run.
pub fn update_history(path: &Path, outcomes: &[Outcome], now: SystemTime) -> Result<()> {
    let mut history: BTreeMap<String, FlakeRecord> = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!(
                "warning: could not read the flaky history {} ({e}); starting a new one",
                path.display()
            );
            BTreeMap::new()
        }),
        _ => BTreeMap::new(),
    };
    let stamp = iso8601(now);
    for o in outcomes.iter().filter(|o| o.status != Status::Skipped) {
        let entry = history.entry(o.full_name.clone()).or_insert(FlakeRecord {
            runs: 0,
            failures: 0,
            flaky_passes: 0,
            last_seen_utc: String::new(),
        });
        entry.runs += 1;
        entry.failures += u64::from(o.status == Status::Failed);
        entry.flaky_passes += u64::from(o.flaky);
        entry.last_seen_utc.clone_from(&stamp);
    }
    write_file(path, &serde_json::to_string_pretty(&history)?)?;
    println!("Flaky history written to {}", path.display());
    Ok(())
}

/// `2026-11-27T15:04:05Z`.
fn iso8601(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &str, status: Status) -> Outcome {
        Outcome {
            full_name: format!("pkg::t::{name}"),
            binary: "pkg::t".into(),
            name: name.into(),
            status,
            duration: Some(Duration::from_millis(1500)),
            attempts: 1,
            flaky: false,
            quarantine: None,
            message: None,
            output: None,
            screenshot: None,
            accessibility: Vec::new(),
        }
    }

    fn sample() -> Vec<Outcome> {
        let mut failed = outcome("fails", Status::Failed);
        failed.message = Some("left != right".into());
        failed.output = Some("panicked: left != right <&>".into());
        let mut flaky = outcome("flaky", Status::Passed);
        flaky.flaky = true;
        flaky.attempts = 2;
        let mut quarantined = outcome("quarantined", Status::Failed);
        quarantined.quarantine = Some("known issue".into());
        vec![
            outcome("passes", Status::Passed),
            failed,
            flaky,
            quarantined,
            outcome("skipped", Status::Skipped),
        ]
    }

    #[test]
    fn reporter_specs_parse() {
        assert_eq!("console".parse::<Reporter>(), Ok(Reporter::Console));
        assert_eq!(
            "junit:out/r.xml".parse::<Reporter>(),
            Ok(Reporter::Junit("out/r.xml".into()))
        );
        assert!("junit".parse::<Reporter>().is_err());
        assert!("trx:x".parse::<Reporter>().is_err());
    }

    #[test]
    fn junit_marks_each_outcome_its_own_way() {
        let outcomes = sample();
        let run = Run {
            summary: Summary::of(&outcomes),
            outcomes: &outcomes,
            elapsed: Duration::from_secs(3),
            shard: Some(Shard { index: 2, total: 4 }),
        };
        let xml = junit(&run);
        assert!(
            xml.contains(r#"<testsuites name="agito" tests="5" failures="1" skipped="1""#),
            "{xml}"
        );
        assert!(xml.contains(r#"<testcase name="passes" classname="pkg::t" time="1.500"/>"#));
        assert!(xml.contains(
            r#"<failure message="left != right">panicked: left != right &lt;&amp;&gt;</failure>"#
        ));
        assert!(xml.contains(r#"<flakyFailure message="passed after 2 attempts"/>"#));
        assert!(xml.contains("QUARANTINED (known issue); failed: failed"));
        assert!(xml.contains("<skipped/>"));
        assert!(xml.contains(r#"<property name="agito.shard.index" value="2"/>"#));
    }

    #[test]
    fn html_lists_failures_first_and_escapes() {
        let outcomes = sample();
        let run = Run {
            summary: Summary::of(&outcomes),
            outcomes: &outcomes,
            elapsed: Duration::from_secs(3),
            shard: None,
        };
        let page = html(&run);
        let fails = page.find("pkg::t::fails").unwrap();
        assert!(fails < page.find("pkg::t::passes").unwrap());
        assert!(page.contains("&lt;&amp;&gt;"));
        assert!(page.contains("<details open>"));
    }

    #[test]
    fn history_accumulates_across_runs() {
        let path = std::env::temp_dir().join(format!("agito-history-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let outcomes = sample();
        let now = UNIX_EPOCH + Duration::from_secs(1_795_000_000);
        update_history(&path, &outcomes, now).unwrap();
        update_history(&path, &outcomes, now).unwrap();
        let history: BTreeMap<String, FlakeRecord> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            history["pkg::t::flaky"],
            FlakeRecord {
                runs: 2,
                failures: 0,
                flaky_passes: 2,
                last_seen_utc: "2026-11-18T11:06:40Z".into()
            }
        );
        assert_eq!(history["pkg::t::fails"].failures, 2);
        assert!(!history.contains_key("pkg::t::skipped"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn timestamps_are_utc_iso8601() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            iso8601(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }
}
