# Command line

The `agito` command comes from the `agito-cli` crate:

```sh
cargo install agito-cli
```

Every command takes `--help`.

## agito install

Downloads [Chrome for Testing](https://developer.chrome.com/blog/chrome-for-testing) into the
browser cache, `~/.agito/browsers`, and makes it the browser agito launches.

```sh
agito install [--revision BUILD] [--path DIR]
```

| Option | Default | Meaning |
|---|---|---|
| `--revision BUILD` | `stable` | Which build: a version (`131.0.6778.85`), a milestone (`131`, its newest build), a Chromium revision (`1368529`), or a channel (`stable`, `beta`, `dev`, `canary`) |
| `--path DIR` | `~/.agito/browsers` | Install somewhere else. The launcher only looks in the default place, so point `AGITO_EXECUTABLE_PATH` at a build installed elsewhere. |
| `--channel` | `chromium` | Only `chromium` is downloaded. Chrome and Edge are found where their own installers put them. |

A build already in the cache is not downloaded again; installing it makes it the one agito
launches. Builds exist for Linux (x64 and Arm), macOS (Intel and Apple silicon) and Windows.

## agito run

Builds the tests with cargo and runs them, with sharding, retries, quarantine and reports.
See [agito run](../running/agito-run.md).

```sh
agito run [OPTIONS] [CARGO_ARGS]... [-- TEST_ARGS...]
```

| Option | Meaning |
|---|---|
| `--shard INDEX/TOTAL` | Run one of `TOTAL` disjoint parts of the suite, such as `2/4` |
| `--workers N\|auto` | Tests run at once in each binary; `auto`, the default, is one per CPU |
| `--reporter SPEC` | Write a report: `junit:PATH` or `html:PATH`. Repeatable. The console report is always shown. |
| `--retries N` | Re-run a failed test up to `N` more times; one that then passes is flaky. Default 0. |
| `--fail-on-flaky` | Fail the run if any test is flaky |
| `--quarantine FILE` | A file of full test names, one per line, whose failures do not fail the run |
| `--flaky-history FILE` | Add this run's results to a JSON history of each test's runs, failures and flaky passes |

`CARGO_ARGS` go to `cargo test --no-run`: `-p`, `--workspace`, `--features`, `--release`,
`--manifest-path` and so on. `TEST_ARGS`, after `--`, select tests: name patterns,
`--exact`, `--skip PATTERN`, `--ignored` and `--include-ignored`.

It exits with status 0 when every test that is not quarantined passed.

## agito shard merge

Merges the JUnit reports of a sharded run. See [Continuous integration](../running/ci.md#sharding-over-several-machines).

```sh
agito shard merge FILES... [--expect N] [--output SPEC]...
```

| Option | Meaning |
|---|---|
| `FILES` | The shards' JUnit reports. A pattern such as `results/shard-*.xml` is expanded even when the shell does not (Windows). |
| `--expect N` | Fail unless the files are shards 1 to `N`, each exactly once |
| `--output SPEC` | Write the merged report: `junit:PATH` or `html:PATH`. Repeatable. |

It prints the combined totals and failures, and exits with a failure status when a test failed
or a shard check failed.

## agito screenshot

Saves a screenshot of a page.

```sh
agito screenshot URL [OPTIONS]
```

| Option | Default | Meaning |
|---|---|---|
| `URL` | | A URL, a local HTML file, or a host name (http for `localhost`, https otherwise) |
| `-o, --output PATH` | `screenshot.png` | Where to save it. The extension picks the format: `.png`, `.jpg`/`.jpeg` or `.webp`. |
| `--full-page` | | Capture the whole scrollable page, not only the viewport |
| `--width N`, `--height N` | 1280, 720 | The viewport, in CSS pixels |
| `--quality N` | | JPEG or WebP quality, 0 to 100 |
| `--wait-until STATE` | `load` | When the page counts as loaded: `load`, `domcontentloaded`, `networkidle` or `commit` |
| `--timeout SECONDS` | 60 | How long the page may take to load |
| `--delay SECONDS` | 0 | Wait after loading, for animations or late content |
| `--hide-banners` | | Remove cookie-consent and privacy banners first |

Scrollbars are hidden, so they do not appear in the capture. A page that answers with an HTTP
error is still captured, with a warning; one that cannot load is an error.

## agito pdf

Saves a page as a PDF. It takes the same `URL`, `--wait-until`, `--timeout`, `--delay` and
`--hide-banners` as `agito screenshot`, and:

| Option | Default | Meaning |
|---|---|---|
| `-o, --output PATH` | `output.pdf` | Where to save it |
| `--width N` | 1440 | The viewport width, which decides the page's responsive layout |
| `--landscape` | | Print in landscape |
| `--background` | | Print background colours and images, which browsers leave out of print by default |

## Browser selection

`agito screenshot` and `agito pdf` launch a browser the way tests do: from
[`agito.config.json`](configuration.md) and `AGITO_*` variables, then the browser `agito
install` installed, then Chrome, Edge or Chromium.
