# Performance

agito reads a page's [Core Web Vitals](https://web.dev/articles/vitals) and runtime counters,
and holds pages to budgets.

| Metric | What it measures | Unit |
|---|---|---|
| `lcp` | Largest Contentful Paint: when the largest element in the viewport rendered | ms |
| `fcp` | First Contentful Paint: when anything first rendered | ms |
| `ttfb` | Time To First Byte: how long the server took to start answering | ms |
| `cls` | Cumulative Layout Shift: how much the page moved under the reader | score |
| `inp` | Interaction to Next Paint: how long the slowest interaction took to show its result | ms |
| `js_heap_size` | The JavaScript heap in use | bytes |
| `dom_nodes` | How many DOM nodes the page holds | count |

Times run from the start of the navigation.

## Reading the metrics

```rust,ignore
page.goto("https://shop.test/").await?;
let metrics = page.performance_metrics().await?;
println!("LCP {:?} ms, CLS {:?}", metrics.lcp, metrics.cls);
for shift in &metrics.layout_shifts {
    println!("moved {:.3} at {:.0} ms: {:?}", shift.score, shift.time, shift.sources);
}
```

The metrics are read from the browser's own record of the page, so there is nothing to turn
on first, and they can be read at any point. A metric the page has not produced yet is `None`:
INP before any interaction, say. Each layout shift names the elements that moved, as CSS
selectors.

- **CLS** is the worst *session window*, as browsers report it: shifts less than a second
  apart, in a window at most five seconds long. Shifts caused by input do not count.
- **INP** is the slowest interaction, ignoring one in fifty when there are many. Browsers keep
  a record only of interactions that took 104 ms or more; below that, the first input stands
  in, which is under any useful INP budget anyway.

## Assertions

LCP and CLS keep changing while a page loads, so the assertions retry until they hold or the
timeout passes, like other assertions:

```rust,ignore
expect(&page).to_have_lcp_below(2500.0).await?;
expect(&page).to_have_fcp_below(1800.0).await?;
expect(&page).to_have_ttfb_below(800.0).await?;
expect(&page).to_have_cls_below(0.1).await?;

page.locator("#checkout").click().await?;
expect(&page).to_have_inp_below(200.0).await?;
```

A per-metric assertion fails while its metric has not been measured, so `to_have_inp_below`
needs an interaction first.

## Budgets

A `PerformanceBudget` sets thresholds for any of the metrics, and
`to_meet_performance_budget_with` checks them all, listing each one that is over:

```rust,ignore
use agito::performance::PerformanceBudget;

let budget = PerformanceBudget {
    lcp: Some(2500.0),
    cls: Some(0.1),
    inp: Some(200.0),
    ..PerformanceBudget::none()
};
expect(&page).to_meet_performance_budget_with(&budget).await?;
```

```text
Assertion to_meet_performance_budget failed after 10000ms. Expected: LCP <= 2500, CLS <= 0.1, INP <= 200. Received: LCP: 3100 ms (budget 2500 ms, over by 600 ms); CLS: 0.020 (budget 0.100); INP: not measured (budget 200 ms). Page: https://shop.test/.
```

In a budget, a metric the page has not produced passes, so one budget fits pages with and
without interactions.

A test can set its own budget, and `to_meet_performance_budget` with no arguments checks it:

```rust,ignore
#[agito::test(perf_budget(lcp = 2500, cls = 0.1, dom_nodes = 1500))]
async fn the_home_page_is_fast(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/").await?;
    expect(&page).to_meet_performance_budget().await?;
    Ok(())
}
```

Without one, it uses the [`performance` section](../reference/configuration.md#performance)
of `agito.config.json`, so a whole suite can share a budget:

```json
{ "performance": { "lcp": 2500, "cls": 0.1, "inp": 200 } }
```

`PerformanceBudget::default()` reads that section too, and `PerformanceBudget::none()` has no
thresholds at all.

Measure against a production-like build: a development server's unminified bundles and
source maps say little about what users will see.
