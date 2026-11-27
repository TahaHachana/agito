# Continuous integration

A CI job for agito tests needs a browser, then runs the suite. Two choices make it
reproducible: install a pinned browser rather than whatever the runner has, and keep the
reports.

## A browser

`agito install` downloads Chrome for Testing to `~/.agito/browsers`, where agito finds it with
no configuration. Pin a milestone or a version, so a Chrome update does not change your
results overnight:

```sh
agito install --revision 131
```

Chromium runs without its sandbox by default, because CI runners and containers often cannot
provide what it needs (recent Ubuntu restricts the user namespaces it uses), and Chromium
refuses to start rather than run without one. Set `AGITO_CHROMIUM_SANDBOX=1` where the sandbox
works.

## One job

```yaml
name: browser tests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install agito-cli --locked
      - run: agito install --revision 131
      - run: agito run --retries 1 --reporter junit:results/junit.xml --reporter html:results/report.html
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: test-results
          path: results/
```

`--retries 1` keeps a test that fails once and then passes from failing the build, while still
reporting it as flaky. Add `--fail-on-flaky` to be strict.

## Sharding over several machines

A large suite can be split across machines. Each job runs one shard with `--shard
INDEX/TOTAL` and writes its own report; a last job merges them:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        shard: [1, 2, 3, 4]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install agito-cli --locked
      - run: agito install --revision 131
      - run: agito run --shard ${{ matrix.shard }}/4 --reporter junit:results/shard-${{ matrix.shard }}.xml
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          name: shard-${{ matrix.shard }}
          path: results/

  report:
    needs: test
    if: always()
    runs-on: ubuntu-latest
    steps:
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install agito-cli --locked
      - uses: actions/download-artifact@v4
        with:
          path: results/
          merge-multiple: true
      - run: agito shard merge "results/shard-*.xml" --expect 4 --output junit:results/all.xml --output html:results/report.html
```

How shards are chosen: every test is sorted by full name, then dealt out in turn, like cards.
That needs no timing data, gives every machine the same answer, and spreads a slow binary's
tests across shards.

`agito shard merge` combines the reports into one, prints the totals and failures, and fails
if any test failed. `--expect 4` also fails it if a shard's report is missing or appears
twice, so a machine that died before writing its report cannot make a partial run look
complete.

## Caching

The build is usually the slowest part of a CI run. Cache cargo's target directory (for
instance with `Swatinem/rust-cache`) and the `agito-cli` install, and keep
`~/.agito/browsers` between runs to skip the browser download.
