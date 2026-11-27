//! Choosing which tests run: filters, ignored tests, shards and the quarantine list.

use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

use super::discover::TestCase;

/// The test arguments after `--`, the subset of libtest's that apply to a whole run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Filters {
    /// A test runs if its full name contains one of these (or equals one, with `--exact`).
    pub patterns: Vec<String>,
    pub exact: bool,
    /// Tests whose full name contains one of these do not run.
    pub skip: Vec<String>,
    /// Run ignored tests too.
    pub include_ignored: bool,
    /// Run only ignored tests.
    pub ignored_only: bool,
}

impl Filters {
    pub fn parse(args: &[String]) -> Result<Filters> {
        let mut filters = Filters::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--exact" => filters.exact = true,
                "--include-ignored" => filters.include_ignored = true,
                "--ignored" => filters.ignored_only = true,
                "--skip" => filters
                    .skip
                    .push(args.next().context("--skip needs a pattern")?.clone()),
                other if other.starts_with('-') => bail!(
                    "agito run does not pass '{other}' to the test binaries; it takes test \
                     name patterns, --exact, --skip, --ignored and --include-ignored"
                ),
                pattern => filters.patterns.push(pattern.to_owned()),
            }
        }
        Ok(filters)
    }

    /// Whether `test` is selected. Patterns match the full name (`binary::test`) or libtest's
    /// name, so both `agito::popups::` and a plain `closes_itself` work.
    pub fn selects(&self, test: &TestCase) -> bool {
        let full = test.full_name();
        let matches = |pattern: &String| {
            if self.exact {
                full == *pattern || test.name == *pattern
            } else {
                full.contains(pattern.as_str())
            }
        };
        let wanted = self.patterns.is_empty() || self.patterns.iter().any(matches);
        let skipped = self.skip.iter().any(|s| full.contains(s.as_str()));
        wanted && !skipped
    }

    /// Whether `test` runs, rather than being reported as skipped.
    pub fn runs(&self, test: &TestCase) -> bool {
        if self.ignored_only {
            test.ignored
        } else {
            !test.ignored || self.include_ignored
        }
    }
}

/// One shard of a run, `index/total` with a 1-based index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shard {
    pub index: usize,
    pub total: usize,
}

impl FromStr for Shard {
    type Err = String;

    fn from_str(spec: &str) -> std::result::Result<Self, String> {
        let invalid = || format!("'{spec}' is not a shard: expected <index>/<total>, such as 1/4");
        let (index, total) = spec.split_once('/').ok_or_else(invalid)?;
        let index: usize = index.trim().parse().map_err(|_| invalid())?;
        let total: usize = total.trim().parse().map_err(|_| invalid())?;
        if total == 0 {
            return Err(format!("'{spec}': the total must be at least 1"));
        }
        if index == 0 || index > total {
            return Err(format!("'{spec}': the index must be between 1 and {total}"));
        }
        Ok(Shard { index, total })
    }
}

impl Shard {
    /// This shard's tests. They are sorted by full name, so every machine sees one order, then
    /// dealt round-robin, which spreads a slow binary's tests across shards without timing data.
    pub fn select(self, mut tests: Vec<TestCase>) -> Vec<TestCase> {
        tests.sort_by_cached_key(TestCase::full_name);
        tests
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % self.total == self.index - 1)
            .map(|(_, t)| t)
            .collect()
    }
}

/// Full test names, one per line, with `#` comments.
pub fn read_quarantine(path: &Path) -> Result<HashSet<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read the quarantine list {}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::super::discover::Binary;
    use super::*;

    fn case(binary: &str, name: &str, ignored: bool) -> TestCase {
        TestCase {
            binary: Arc::new(Binary {
                id: binary.into(),
                package: "p".into(),
                crate_name: "p".into(),
                path: PathBuf::new(),
                manifest_dir: PathBuf::new(),
                library_path: None,
            }),
            name: name.into(),
            ignored,
        }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filters_match_full_or_libtest_names() {
        let filters = Filters::parse(&args(&["agito::popups::", "--skip", "slow"])).unwrap();
        assert!(filters.selects(&case("agito::popups", "closes", false)));
        assert!(!filters.selects(&case("agito::popups", "slow_child", false)));
        assert!(!filters.selects(&case("agito::files", "closes", false)));

        let exact = Filters::parse(&args(&["closes", "--exact"])).unwrap();
        assert!(exact.selects(&case("agito::popups", "closes", false)));
        assert!(!exact.selects(&case("agito::popups", "closes_too", false)));
        assert!(Filters::default().selects(&case("x", "y", false)));
        assert!(Filters::parse(&args(&["--nocapture"])).is_err());
    }

    #[test]
    fn ignored_tests_run_only_when_asked() {
        let (plain, ignored) = (case("b", "a", false), case("b", "i", true));
        let default = Filters::default();
        assert!(default.runs(&plain) && !default.runs(&ignored));
        let include = Filters::parse(&args(&["--include-ignored"])).unwrap();
        assert!(include.runs(&plain) && include.runs(&ignored));
        let only = Filters::parse(&args(&["--ignored"])).unwrap();
        assert!(!only.runs(&plain) && only.runs(&ignored));
    }

    #[test]
    fn shards_are_disjoint_and_cover_everything() {
        let tests: Vec<TestCase> = (0..10)
            .map(|i| case(if i % 2 == 0 { "a" } else { "b" }, &format!("t{i}"), false))
            .collect();
        let mut seen = Vec::new();
        for index in 1..=3 {
            let mut shuffled = tests.clone();
            shuffled.reverse();
            let shard = Shard { index, total: 3 }.select(shuffled);
            assert!(shard.len() >= 3);
            seen.extend(shard.iter().map(TestCase::full_name));
        }
        seen.sort();
        let mut all: Vec<String> = tests.iter().map(TestCase::full_name).collect();
        all.sort();
        assert_eq!(seen, all);
    }

    #[test]
    fn shard_specs_parse_and_validate() {
        assert_eq!("2/4".parse::<Shard>(), Ok(Shard { index: 2, total: 4 }));
        for bad in ["0/4", "5/4", "1/0", "1", "a/b", ""] {
            assert!(bad.parse::<Shard>().is_err(), "{bad}");
        }
    }
}
