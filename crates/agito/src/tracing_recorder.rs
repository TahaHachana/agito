//! Tracing: recording what a context did, to see afterwards why a test went the way it did.
//!
//! A trace is a zip holding:
//!
//! - `trace.json`: the event log. Each action with its target, duration and error, each
//!   navigation with its status, pages opening and closing, console messages, uncaught errors
//!   and dialogs, timed from the start of the trace.
//! - `har.json`: the network log of every page, in HAR format.
//! - `resources/screenshots/NNNNNN.jpeg`: a screenshot after each action and navigation, which
//!   the event it followed names.
//! - `chrome-trace.json`, when asked for: the browser's performance timeline, for Chrome
//!   DevTools or Perfetto. Chrome records one at a time per browser, and the timeline covers
//!   the whole browser, so it is off by default.
//!
//! Each context traces on its own, so tests running in parallel trace at once.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use agito_protocol::cdp::{io, tracing as cdp_tracing};
use base64::Engine;
use serde_json::{Value, json};

use crate::context::BrowserContext;
use crate::error::{Error, Result};
use crate::page::{ImageFormat, Page, Response, ScreenshotOptions};
use crate::plugin::ActionResult;

/// How long a screenshot may take before it is skipped: a page stopped on a dialog cannot be
/// captured at all.
const SCREENSHOT_TIMEOUT: Duration = Duration::from_millis(1500);

/// The categories a performance timeline records, as Chrome DevTools' Performance panel does.
const TIMELINE_CATEGORIES: &[&str] = &[
    "-*",
    "devtools.timeline",
    "v8.execute",
    "disabled-by-default-devtools.timeline",
    "disabled-by-default-devtools.timeline.frame",
    "toplevel",
    "blink.console",
    "blink.user_timing",
    "latencyInfo",
    "disabled-by-default-devtools.timeline.stack",
    "disabled-by-default-v8.cpu_profiler",
];

/// What a trace records.
#[derive(Debug, Clone)]
pub struct TracingOptions {
    /// A name for the trace, written into `trace.json`.
    pub title: Option<String>,
    /// Take a screenshot after each action and navigation. Default: true.
    pub screenshots: bool,
    /// Also record the browser's performance timeline as `chrome-trace.json`. Chrome records
    /// one at a time per browser, so a second context asking waits for the first to stop.
    /// Default: false.
    pub timeline: bool,
}

impl Default for TracingOptions {
    fn default() -> Self {
        TracingOptions {
            title: None,
            screenshots: true,
            timeline: false,
        }
    }
}

/// A running performance timeline: the browser-wide permit, and the browser session.
struct Timeline {
    _permit: tokio::sync::OwnedMutexGuard<()>,
    session: Arc<dyn crate::transport::Session>,
    complete: crate::transport::EventStream,
}

/// A trace being recorded for one context.
pub(crate) struct Recorder {
    options: TracingOptions,
    started: Instant,
    started_at: SystemTime,
    events: Mutex<Vec<Value>>,
    screenshots: Mutex<Vec<Vec<u8>>>,
    /// Every page seen, with its label, held so its network log outlives it.
    pages: Mutex<Vec<(Page, String)>>,
    /// When the action or navigation running on each page (by target id) began.
    running: Mutex<HashMap<String, Instant>>,
    timeline: tokio::sync::Mutex<Option<Timeline>>,
}

impl Recorder {
    fn new(options: TracingOptions) -> Recorder {
        Recorder {
            options,
            started: Instant::now(),
            started_at: SystemTime::now(),
            events: Mutex::default(),
            screenshots: Mutex::default(),
            pages: Mutex::default(),
            running: Mutex::default(),
            timeline: tokio::sync::Mutex::new(None),
        }
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    fn push(&self, mut event: Value) {
        event["time"] = json!(round(self.now()));
        self.events.lock().unwrap().push(event);
    }

    /// The label a page has in the trace, `page-1` and so on, giving it one if it is new.
    fn label(&self, page: &Page) -> String {
        let mut pages = self.pages.lock().unwrap();
        if let Some((_, label)) = pages
            .iter()
            .find(|(p, _)| p.target_id() == page.target_id())
        {
            return label.clone();
        }
        let label = format!("page-{}", pages.len() + 1);
        page.start_har();
        pages.push((page.clone(), label.clone()));
        label
    }

    pub(crate) fn page_opened(&self, page: &Page) {
        let label = self.label(page);
        self.push(json!({ "type": "page", "event": "opened", "page": label, "url": page.url() }));
    }

    pub(crate) fn page_closed(&self, page: &Page) {
        let label = self.label(page);
        self.push(json!({ "type": "page", "event": "closed", "page": label }));
    }

    pub(crate) fn started(&self, page: &Page) {
        self.running
            .lock()
            .unwrap()
            .insert(page.target_id().to_owned(), Instant::now());
    }

    fn duration(&self, page: &Page) -> Option<f64> {
        self.running
            .lock()
            .unwrap()
            .remove(page.target_id())
            .map(|t| round(t.elapsed().as_secs_f64() * 1000.0))
    }

    pub(crate) async fn action_finished(&self, page: &Page, result: &ActionResult) {
        let label = self.label(page);
        let duration = self.duration(page);
        let screenshot = self.screenshot(page).await;
        self.push(json!({
            "type": "action",
            "page": label,
            "action": result.action,
            "target": result.target,
            "duration": duration,
            "error": result.error,
            "screenshot": screenshot,
        }));
    }

    pub(crate) async fn navigated(&self, page: &Page, url: &str, response: Option<&Response>) {
        let label = self.label(page);
        let duration = self.duration(page);
        let screenshot = self.screenshot(page).await;
        self.push(json!({
            "type": "navigation",
            "page": label,
            "url": url,
            "status": response.map(Response::status),
            "duration": duration,
            "screenshot": screenshot,
        }));
    }

    pub(crate) fn console(&self, page: &Page, kind: &str, text: &str) {
        let label = self.label(page);
        self.push(json!({ "type": "console", "page": label, "level": kind, "text": text }));
    }

    pub(crate) fn page_error(&self, page: &Page, text: &str) {
        let label = self.label(page);
        self.push(json!({ "type": "pageError", "page": label, "text": text }));
    }

    pub(crate) fn dialog(&self, page: &Page, kind: &str, message: &str) {
        let label = self.label(page);
        self.push(json!({ "type": "dialog", "page": label, "kind": kind, "message": message }));
    }

    /// A screenshot of `page` as it is now, stored, and the path the trace gives it.
    async fn screenshot(&self, page: &Page) -> Option<String> {
        if !self.options.screenshots || page.is_closed() {
            return None;
        }
        let options = ScreenshotOptions {
            format: ImageFormat::Jpeg,
            quality: Some(60),
            ..Default::default()
        };
        let bytes = tokio::time::timeout(SCREENSHOT_TIMEOUT, page.screenshot(options))
            .await
            .ok()?
            .ok()?;
        let mut shots = self.screenshots.lock().unwrap();
        shots.push(bytes);
        Some(screenshot_path(shots.len()))
    }
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}

fn screenshot_path(number: usize) -> String {
    format!("resources/screenshots/{number:06}.jpeg")
}

impl BrowserContext {
    /// Starts recording a trace of this context: its actions, navigations, console, network
    /// and screenshots. Stop it with [`stop_tracing`](Self::stop_tracing) to save it, or
    /// [`discard_tracing`](Self::discard_tracing).
    ///
    /// ```no_run
    /// # async fn example(context: agito::BrowserContext, page: agito::Page) -> agito::Result<()> {
    /// context.start_tracing(Default::default()).await?;
    /// page.goto("https://shop.test/").await?;
    /// page.locator("text=Buy").click().await?;
    /// context.stop_tracing("trace.zip").await?;
    /// # Ok(()) }
    /// ```
    pub async fn start_tracing(&self, options: TracingOptions) -> Result<()> {
        if self.inner.tracer.lock().unwrap().is_some() {
            return Err(Error::invalid_argument(
                "tracing is already running in this context",
            ));
        }
        let timeline = options.timeline;
        let recorder = Arc::new(Recorder::new(options));
        if timeline {
            *recorder.timeline.lock().await = Some(start_timeline(self).await?);
        }
        for page in self.pages() {
            recorder.label(&page);
        }
        if let Some(title) = &recorder.options.title {
            recorder.push(json!({ "type": "trace", "title": title }));
        }
        *self.inner.tracer.lock().unwrap() = Some(recorder);
        Ok(())
    }

    /// Whether a trace is being recorded.
    pub fn is_tracing(&self) -> bool {
        self.inner.tracer.lock().unwrap().is_some()
    }

    /// Stops the trace and writes it to `path` as a zip.
    pub async fn stop_tracing(&self, path: impl AsRef<Path>) -> Result<()> {
        let recorder = self.take_recorder()?;
        let timeline = match recorder.timeline.lock().await.take() {
            Some(timeline) => Some(stop_timeline(timeline).await?),
            None => None,
        };
        write_trace(path.as_ref(), &recorder, timeline)
    }

    /// Stops the trace without saving it.
    pub async fn discard_tracing(&self) -> Result<()> {
        let recorder = self.take_recorder()?;
        if let Some(timeline) = recorder.timeline.lock().await.take() {
            let _ = stop_timeline(timeline).await;
        }
        for (page, _) in recorder.pages.lock().unwrap().iter() {
            page.stop_har();
        }
        Ok(())
    }

    fn take_recorder(&self) -> Result<Arc<Recorder>> {
        self.inner
            .tracer
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| Error::invalid_argument("no trace is running; call start_tracing first"))
    }

    /// The trace being recorded, if any.
    pub(crate) fn recorder(&self) -> Option<Arc<Recorder>> {
        self.inner.tracer.lock().unwrap().clone()
    }
}

async fn start_timeline(context: &BrowserContext) -> Result<Timeline> {
    let browser = context.browser();
    // One timeline per browser: a second waits for the first to stop.
    let permit = browser.inner.timeline.clone().lock_owned().await;
    let session = browser.browser_session();
    let complete = session.subscribe(&["Tracing.tracingComplete"]);
    session
        .send(&cdp_tracing::StartParams {
            transfer_mode: Some(cdp_tracing::StartParamsTransferMode::ReturnAsStream),
            trace_config: Some(cdp_tracing::TraceConfig {
                included_categories: Some(
                    TIMELINE_CATEGORIES
                        .iter()
                        .map(|c| (*c).to_owned())
                        .collect(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;
    Ok(Timeline {
        _permit: permit,
        session,
        complete,
    })
}

/// Ends the timeline and reads it back: the trace events, as JSON.
async fn stop_timeline(mut timeline: Timeline) -> Result<Vec<u8>> {
    let session = timeline.session.clone();
    session.send(&cdp_tracing::EndParams::default()).await?;
    let event = tokio::time::timeout(Duration::from_secs(30), timeline.complete.next())
        .await
        .map_err(|_| Error::other("The browser did not finish the performance timeline in 30s."))?
        .ok_or_else(|| Error::other("The browser went away while finishing the timeline."))?;
    let complete: cdp_tracing::TracingCompleteEvent = serde_json::from_value(event.params)
        .map_err(|e| Error::other(format!("Could not read the timeline's completion: {e}")))?;
    let Some(handle) = complete.stream else {
        return Ok(b"[]".to_vec());
    };
    let mut data = Vec::new();
    let read = async {
        loop {
            let chunk = session
                .send(&io::ReadParams {
                    handle: handle.clone(),
                    ..Default::default()
                })
                .await?;
            if chunk.base64_encoded == Some(true) {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&chunk.data)
                    .map_err(|e| Error::other(format!("The timeline was not base64: {e}")))?;
                data.extend(bytes);
            } else {
                data.extend(chunk.data.as_bytes());
            }
            if chunk.eof {
                break;
            }
        }
        Ok::<(), Error>(())
    };
    let outcome = read.await;
    let _ = session.send(&io::CloseParams { handle }).await;
    outcome?;
    Ok(data)
}

/// Writes the zip: the event log, the network log, the screenshots, and the timeline.
fn write_trace(path: &Path, recorder: &Recorder, timeline: Option<Vec<u8>>) -> Result<()> {
    let io_error =
        |e: std::io::Error| Error::io(format!("writing the trace {}", path.display()), e);
    let zip_error = |e: zip::result::ZipError| {
        Error::other(format!("writing the trace {}: {e}", path.display()))
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(io_error)?;
    }

    let pages = recorder.pages.lock().unwrap().clone();
    let started_at = recorder
        .started_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    let log = json!({
        "version": 1,
        "title": recorder.options.title,
        "startedAt": started_at,
        "duration": round(recorder.now()),
        "pages": pages.iter().map(|(_, label)| label).collect::<Vec<_>>(),
        "events": *recorder.events.lock().unwrap(),
    });

    // One HAR for the context: the first page's log, with every page's entries.
    let mut har: Option<Value> = None;
    for (page, label) in &pages {
        let Some(mut log) = page.stop_har() else {
            continue;
        };
        if let Some(entries) = log["log"]["entries"].as_array_mut() {
            for entry in entries.iter_mut() {
                entry["pageref"] = json!(label);
            }
        }
        match &mut har {
            None => har = Some(log),
            Some(all) => {
                let more = log["log"]["entries"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if let Some(entries) = all["log"]["entries"].as_array_mut() {
                    entries.extend(more);
                }
            }
        }
    }

    let file = std::fs::File::create(path).map_err(io_error)?;
    let mut zip = zip::ZipWriter::new(file);
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("trace.json", deflated).map_err(zip_error)?;
    zip.write_all(
        serde_json::to_string_pretty(&log)
            .expect("the log serializes")
            .as_bytes(),
    )
    .map_err(io_error)?;
    if let Some(har) = har {
        zip.start_file("har.json", deflated).map_err(zip_error)?;
        zip.write_all(
            serde_json::to_string(&har)
                .expect("HAR serializes")
                .as_bytes(),
        )
        .map_err(io_error)?;
    }
    for (i, shot) in recorder.screenshots.lock().unwrap().iter().enumerate() {
        zip.start_file(screenshot_path(i + 1), stored)
            .map_err(zip_error)?;
        zip.write_all(shot).map_err(io_error)?;
    }
    if let Some(timeline) = timeline {
        zip.start_file("chrome-trace.json", deflated)
            .map_err(zip_error)?;
        zip.write_all(&timeline).map_err(io_error)?;
    }
    zip.finish().map_err(zip_error)?;
    Ok(())
}
