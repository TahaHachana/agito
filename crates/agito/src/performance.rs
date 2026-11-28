//! Performance: Core Web Vitals and runtime metrics, and budgets to hold them to.
//!
//! Metrics are read on demand from the browser's own buffered performance entries, so nothing
//! has to be set up before a page loads:
//!
//! - **LCP**, Largest Contentful Paint: when the largest element in the viewport rendered.
//! - **FCP**, First Contentful Paint: when anything first rendered.
//! - **TTFB**, Time To First Byte: how long the server took to start answering.
//! - **CLS**, Cumulative Layout Shift: how much the page moved under the reader, as the worst
//!   session window (shifts under a second apart, at most five seconds long).
//! - **INP**, Interaction to Next Paint: how long the slowest interaction took to show its
//!   result, ignoring one in fifty when there are many. The browser buffers only interactions
//!   of 104 ms or more, so below that the first input stands in.
//!
//! Times are milliseconds from the start of the navigation; CLS has no unit.

use std::fmt;

use agito_protocol::cdp::performance as cdp_performance;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::Result;
use crate::page::Page;

const CSS_PATH: &str = include_str!("../js/css_path.js");
const COLLECT: &str = include_str!("../js/performance_metrics.js");

/// One layout shift the page made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutShift {
    /// How much it moved the page, as the browser scores it.
    pub score: f64,
    /// When it happened, in milliseconds from the start of the navigation.
    pub time: f64,
    /// CSS selectors for the elements that moved, where the browser named them.
    pub sources: Vec<String>,
}

/// A page's performance, as [`Page::performance_metrics`] reads it. A metric the page has
/// not produced yet, such as INP before any interaction, is `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceMetrics {
    /// Largest Contentful Paint, in milliseconds.
    pub lcp: Option<f64>,
    /// First Contentful Paint, in milliseconds.
    pub fcp: Option<f64>,
    /// Time To First Byte, in milliseconds.
    pub ttfb: Option<f64>,
    /// Cumulative Layout Shift.
    pub cls: Option<f64>,
    /// Interaction to Next Paint, in milliseconds.
    pub inp: Option<f64>,
    /// The JavaScript heap in use, in bytes.
    pub js_heap_size: Option<u64>,
    /// How many DOM nodes the page holds.
    pub dom_nodes: Option<u64>,
    /// Each layout shift not caused by input, in order.
    pub layout_shifts: Vec<LayoutShift>,
}

/// What the collector script returns.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vitals {
    lcp: Option<f64>,
    fcp: Option<f64>,
    ttfb: Option<f64>,
    cls: Option<f64>,
    inp: Option<f64>,
    layout_shifts: Vec<LayoutShift>,
}

impl Page {
    /// The page's Core Web Vitals and runtime metrics, as they stand now. LCP and CLS keep
    /// changing while a page loads; [`expect`](crate::expect())'s performance assertions retry
    /// until they hold.
    ///
    /// ```no_run
    /// # async fn example(page: agito::Page) -> agito::Result<()> {
    /// page.goto("https://shop.test/").await?;
    /// let metrics = page.performance_metrics().await?;
    /// println!("LCP {:?} ms, CLS {:?}", metrics.lcp, metrics.cls);
    /// # Ok(()) }
    /// ```
    pub async fn performance_metrics(&self) -> Result<PerformanceMetrics> {
        let script = format!("(() => {{\n{CSS_PATH}\nreturn (\n{COLLECT}\n);\n}})()");
        let vitals: Vitals = self.evaluate(&script).await?;
        let mut metrics = PerformanceMetrics {
            lcp: vitals.lcp,
            fcp: vitals.fcp,
            ttfb: vitals.ttfb,
            cls: vitals.cls,
            inp: vitals.inp,
            layout_shifts: vitals.layout_shifts,
            ..Default::default()
        };
        // Runtime counters come from the Performance domain; a page that cannot give them
        // still has its vitals.
        let session = self.main_frame().session();
        if session
            .send(&cdp_performance::EnableParams::default())
            .await
            .is_ok()
            && let Ok(runtime) = session
                .send(&cdp_performance::GetMetricsParams::default())
                .await
        {
            for metric in runtime.metrics {
                match metric.name.as_str() {
                    "JSHeapUsedSize" => metrics.js_heap_size = Some(metric.value as u64),
                    "Nodes" => metrics.dom_nodes = Some(metric.value as u64),
                    _ => {}
                }
            }
        }
        Ok(metrics)
    }
}

/// Thresholds a page's metrics must stay at or under. Unset thresholds are not checked.
///
/// [`Default`] reads the `performance` section of `agito.config.json`; with none, nothing is
/// checked. A test can set its own with `#[agito::test(perf_budget(lcp = 2500, cls = 0.1))]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceBudget {
    /// Largest Contentful Paint, in milliseconds.
    pub lcp: Option<f64>,
    /// First Contentful Paint, in milliseconds.
    pub fcp: Option<f64>,
    /// Time To First Byte, in milliseconds.
    pub ttfb: Option<f64>,
    /// Cumulative Layout Shift.
    pub cls: Option<f64>,
    /// Interaction to Next Paint, in milliseconds.
    pub inp: Option<f64>,
    /// The JavaScript heap in use, in bytes.
    pub js_heap_size: Option<u64>,
    /// How many DOM nodes the page holds.
    pub dom_nodes: Option<u64>,
}

impl Default for PerformanceBudget {
    fn default() -> Self {
        let c = &config::get().performance;
        PerformanceBudget {
            lcp: c.lcp,
            fcp: c.fcp,
            ttfb: c.ttfb,
            cls: c.cls,
            inp: c.inp,
            js_heap_size: c.js_heap_size,
            dom_nodes: c.dom_nodes,
        }
    }
}

/// One metric checked against its threshold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetCheck {
    /// The metric's name, such as `LCP`.
    pub metric: &'static str,
    /// Its threshold.
    pub threshold: f64,
    /// What the page measured; `None` when it has not produced the metric, which passes.
    pub actual: Option<f64>,
    /// Whether it is at or under the threshold.
    pub passed: bool,
}

impl fmt::Display for BudgetCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = |v: f64| match self.metric {
            "CLS" => format!("{v:.3}"),
            "JS heap" => format!("{v:.0} bytes"),
            "DOM nodes" => format!("{v:.0}"),
            _ => format!("{v:.0} ms"),
        };
        match self.actual {
            Some(actual) => write!(
                f,
                "{}: {} (budget {}{})",
                self.metric,
                unit(actual),
                unit(self.threshold),
                if self.passed {
                    String::new()
                } else {
                    format!(", over by {}", unit(actual - self.threshold))
                }
            ),
            None => write!(
                f,
                "{}: not measured (budget {})",
                self.metric,
                unit(self.threshold)
            ),
        }
    }
}

impl PerformanceBudget {
    /// A budget with no thresholds, whatever the configuration says.
    pub fn none() -> Self {
        PerformanceBudget {
            lcp: None,
            fcp: None,
            ttfb: None,
            cls: None,
            inp: None,
            js_heap_size: None,
            dom_nodes: None,
        }
    }

    /// Whether any threshold is set.
    pub fn is_empty(&self) -> bool {
        self.checks(&PerformanceMetrics::default()).is_empty()
    }

    /// Each set threshold, checked against `metrics`. A metric the page has not produced
    /// passes: INP, say, before anything was clicked.
    pub fn checks(&self, metrics: &PerformanceMetrics) -> Vec<BudgetCheck> {
        let as_f64 = |v: Option<u64>| v.map(|v| v as f64);
        [
            ("LCP", self.lcp, metrics.lcp),
            ("FCP", self.fcp, metrics.fcp),
            ("TTFB", self.ttfb, metrics.ttfb),
            ("CLS", self.cls, metrics.cls),
            ("INP", self.inp, metrics.inp),
            (
                "JS heap",
                as_f64(self.js_heap_size),
                as_f64(metrics.js_heap_size),
            ),
            (
                "DOM nodes",
                as_f64(self.dom_nodes),
                as_f64(metrics.dom_nodes),
            ),
        ]
        .into_iter()
        .filter_map(|(metric, threshold, actual)| {
            let threshold = threshold?;
            Some(BudgetCheck {
                metric,
                threshold,
                actual,
                passed: actual.is_none_or(|a| a <= threshold),
            })
        })
        .collect()
    }
}

tokio::task_local! {
    /// The budget the running `#[agito::test]` set with `perf_budget(...)`.
    pub(crate) static TEST_BUDGET: PerformanceBudget;
}

/// The budget in force: the running test's, or the configuration's.
pub(crate) fn current_budget() -> PerformanceBudget {
    TEST_BUDGET.try_with(Clone::clone).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_check_only_what_they_set() {
        let budget = PerformanceBudget {
            lcp: Some(2500.0),
            cls: Some(0.1),
            inp: Some(200.0),
            dom_nodes: Some(1500),
            ..PerformanceBudget::none()
        };
        let metrics = PerformanceMetrics {
            lcp: Some(3100.0),
            cls: Some(0.05),
            dom_nodes: Some(1500),
            ..Default::default()
        };
        let checks = budget.checks(&metrics);
        let lines: Vec<String> = checks.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            [
                "LCP: 3100 ms (budget 2500 ms, over by 600 ms)",
                "CLS: 0.050 (budget 0.100)",
                "INP: not measured (budget 200 ms)",
                "DOM nodes: 1500 (budget 1500)",
            ]
        );
        assert_eq!(
            checks.iter().filter(|c| !c.passed).count(),
            1,
            "only LCP fails; an unmeasured INP and a metric at its threshold pass"
        );
        assert!(PerformanceBudget::none().is_empty());
        assert!(!budget.is_empty());
    }
}
