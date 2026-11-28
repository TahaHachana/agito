# agito run

`cargo test` runs agito tests fine. `agito run` runs the same tests with what a growing suite
needs around them: sharding across machines, retries that tell flaky tests apart, quarantine,
and JUnit and HTML reports.

```sh
cargo install agito-cli
agito run
```

## How it works

`agito run` builds the tests with `cargo test --no-run`, lists the tests in each test binary,
then runs the binaries itself, one after another, each with its tests in parallel threads.
It gives each binary what `cargo test` would: the package's directory as the working
directory, `CARGO_MANIFEST_DIR`, and the library path proc-macro crates' tests need.

Running the binaries itself is what makes the rest possible: every result is tied to the
binary it came from, a failed test can be re-run on its own, and a shard is an exact list of
tests. cargo-nextest works the same way.

It runs plain `#[test]`s as well as `#[agito::test]`s, so it can run a whole workspace. It
does not run doctests; run those with `cargo test --doc`.

## Choosing what to run

Arguments for cargo come first, then `--` and arguments for the tests:

```sh
agito run -p shop --features e2e -- checkout
agito run --workspace -- --skip slow
```

Before `--` go cargo's arguments, as `cargo test` takes them: `-p`, `--workspace`,
`--features`, `--release`, `--manifest-path` and so on. After it:

| Argument | Selects |
|---|---|
| `pattern` | Tests whose full name contains the pattern. Several patterns select tests matching any. |
| `--exact` | Patterns must match a name exactly |
| `--skip pattern` | Leaves out tests whose name contains the pattern; repeatable |
| `--ignored` | Only `#[ignore]`d tests |
| `--include-ignored` | `#[ignore]`d tests as well |

A test's full name is its binary followed by its name: `shop::checkout::pays_by_card` is the
test `pays_by_card` in the `checkout` module of the `shop` package's unit tests, and
`shop::login::signs_in` the test `signs_in` in `tests/login.rs`. Patterns match either the
full name or the test's own name.

`--workers N` sets how many tests run at once in each binary (libtest's `--test-threads`); the
default is one per CPU.

## Output and reports

`agito run` prints each test as it finishes, with its duration for `#[agito::test]`s, then
every failure with its output and the totals:

```text
Running 41 tests from 3 binaries
  PASS  shop::login::signs_in (0.81s)
  FAIL  shop::checkout::pays_by_card (2.40s)
  FLAKY shop::search::finds_the_lamp (0.62s)
  QUAR  shop::recommendations::load (0.33s)
...
Results: 39 passed, 1 failed, 1 flaky, 1 quarantined, 1 skipped, 42 total (18.20s)
```

`--reporter` adds a report file, and can be repeated:

- `--reporter junit:results/junit.xml` writes JUnit XML, which CI systems display. Each
  test binary is a `<testsuite>`; a flaky pass carries a `<flakyFailure>`, and a quarantined
  failure is noted without failing. A failure's screenshot and trace are attachments, in the
  `[[ATTACHMENT|path]]` form Jenkins and GitLab read.
- `--reporter html:results/report.html` writes a single page that works offline: the totals,
  then every test, failures first with their output, failure screenshot and a link to the
  [trace](../writing/tracing.md#keeping-the-traces-of-failed-tests).

The run exits with a failure code if any test failed that is not quarantined.

## Retries and flaky tests

`--retries N` re-runs each failed test, on its own, up to `N` more times. A test that fails
and then passes is reported as **flaky**: it does not fail the run, but it is counted and
listed apart.

| Option | Effect |
|---|---|
| `--retries N` | Re-run failures up to `N` times |
| `--fail-on-flaky` | Fail the run if any test is flaky |
| `--flaky-history FILE` | Add this run to a JSON file of each test's runs, failures and flaky passes |

The history file accumulates across runs, so keeping it (as a CI cache, for instance) shows
which tests fail most over time.

## Quarantine

`--quarantine FILE` reads a list of full test names, one per line, with `#` comments:

```text
# Fails on the CI runners' fonts; see issue 41.
shop::visual::header_matches
```

Listed tests still run and are reported, but their failures do not fail the run. Tests
quarantined in code with [`#[agito::test(quarantine = "...")]`](../writing/flaky-tests.md#quarantine)
are reported the same way.

## Accessibility

`--a11y warn` audits every page the tests visit for
[accessibility](../writing/accessibility.md) and reports what it finds; `--a11y enforce`
also fails the tests that leave violations behind. Violations are listed under their test,
counted in the summary, and kept in the reports: as notes in JUnit, where they never fail a
test themselves, and in the HTML report's details.

```text
  PASS  shop::checkout::pays_by_card (2.40s)
        A11Y [error] a11y-unlabeled-form-control: Form control (role: textbox) has no accessible name. ... (#card)
...
Accessibility: 1 violation(s) in 1 test
```

## When a test binary crashes

If a binary dies part-way, from a segfault or an `abort()`, the tests it had not reported are
run again one at a time, so the crash is blamed on the test that caused it and the others get
a real result.

## Sharding

`--shard 2/4` runs the second of four disjoint parts of the suite. The four shards together
run every test exactly once; see [Continuous integration](ci.md) for spreading them over
machines and merging their reports.
