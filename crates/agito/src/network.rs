//! Network: requests and responses as they happen, routing (interception and mocking), and
//! HAR recording.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agito_protocol::cdp::{fetch, network};
use base64::Engine;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::error::{Error, ErrorKind, Result};
use crate::frame::Frame;
use crate::page::{Page, PageInner, UrlMatcher};
use crate::transport::Session;

/// How long `body()` waits for a response to finish loading.
const BODY_TIMEOUT: Duration = Duration::from_secs(30);

fn header_map(headers: &Value) -> HashMap<String, String> {
    headers
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    (
                        k.to_ascii_lowercase(),
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resource_type_name(t: Option<network::ResourceType>) -> String {
    t.and_then(|t| serde_json::to_value(t).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "Other".into())
}

/// Where a request is in its life.
#[derive(Debug, Clone, PartialEq)]
enum RequestState {
    InFlight,
    Finished,
    Failed(String),
}

struct RequestData {
    id: String,
    url: String,
    method: String,
    headers: HashMap<String, String>,
    post_data: Option<String>,
    resource_type: String,
    frame_id: Option<String>,
    page: Weak<PageInner>,
    response: Mutex<Option<Arc<ResponseData>>>,
    state: watch::Sender<RequestState>,
}

/// An HTTP request the page made. Cheap to clone.
#[derive(Clone)]
pub struct Request {
    inner: Arc<RequestData>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Request({} {})", self.inner.method, self.inner.url)
    }
}

impl Request {
    pub(crate) fn from_cdp(
        id: String,
        request: &network::Request,
        resource_type: Option<network::ResourceType>,
        frame_id: Option<String>,
        page: Weak<PageInner>,
    ) -> Self {
        let (state, _) = watch::channel(RequestState::InFlight);
        Self {
            inner: Arc::new(RequestData {
                id,
                url: request.url.clone(),
                method: request.method.clone(),
                headers: header_map(&request.headers),
                post_data: request.post_data.clone(),
                resource_type: resource_type_name(resource_type),
                frame_id,
                page,
                response: Mutex::new(None),
                state,
            }),
        }
    }

    /// The browser's id for the request (shared by every hop of a redirect chain).
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// The request's URL.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// The HTTP method, such as `GET`.
    pub fn method(&self) -> &str {
        &self.inner.method
    }

    /// Request headers, with lowercase names.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.inner.headers
    }

    /// One header, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner
            .headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The request body, if it has one.
    pub fn post_data(&self) -> Option<&str> {
        self.inner.post_data.as_deref()
    }

    /// The request body parsed as JSON.
    pub fn post_data_json<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        self.inner
            .post_data
            .as_deref()
            .map(|body| {
                serde_json::from_str(body).map_err(|e| {
                    Error::other(format!("request body is not the expected JSON: {e}"))
                })
            })
            .transpose()
    }

    /// What the browser is loading: `Document`, `Script`, `XHR`, `Fetch`, `Image`, ...
    pub fn resource_type(&self) -> &str {
        &self.inner.resource_type
    }

    /// Whether this request loads a frame's document.
    pub fn is_navigation_request(&self) -> bool {
        self.inner.resource_type == "Document"
    }

    /// The frame that made the request, while it is attached.
    pub fn frame(&self) -> Option<Frame> {
        let page = Page::from_inner(self.inner.page.upgrade()?);
        page.frame_by_id(self.inner.frame_id.as_deref()?)
    }

    /// The response, once headers have arrived.
    pub fn response(&self) -> Option<Response> {
        let data = self.inner.response.lock().unwrap().clone()?;
        Some(Response {
            data,
            request: Some(self.clone()),
        })
    }

    /// Why the request failed, if it did (`net::ERR_FAILED`, ...).
    pub fn failure(&self) -> Option<String> {
        match &*self.inner.state.borrow() {
            RequestState::Failed(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    pub(crate) fn set_response(&self, data: Arc<ResponseData>) {
        *self.inner.response.lock().unwrap() = Some(data);
    }

    pub(crate) fn finish(&self, state: RequestStateUpdate) {
        let state = match state {
            RequestStateUpdate::Finished => RequestState::Finished,
            RequestStateUpdate::Failed(reason) => RequestState::Failed(reason),
        };
        self.inner.state.send_replace(state);
    }

    /// Waits until the request has finished loading or failed.
    async fn settled(&self, timeout: Duration) -> Result<()> {
        let mut rx = self.inner.state.subscribe();
        let wait = async {
            loop {
                match &*rx.borrow_and_update() {
                    RequestState::Finished => return Ok(()),
                    RequestState::Failed(reason) => {
                        return Err(Error::other(format!(
                            "{} {} failed: {reason}",
                            self.inner.method, self.inner.url
                        )));
                    }
                    RequestState::InFlight => {}
                }
                if rx.changed().await.is_err() {
                    return Ok(());
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or_else(|_| {
                Err(ErrorKind::WaitTimeout {
                    condition: "response body".into(),
                    timeout,
                    last_evaluated_value: None,
                    message: format!(
                        "{} did not finish loading within {}ms.",
                        self.inner.url,
                        timeout.as_millis()
                    ),
                }
                .into())
            })
    }
}

/// How a request ended.
pub(crate) enum RequestStateUpdate {
    Finished,
    Failed(String),
}

pub(crate) struct ResponseData {
    url: String,
    status: u16,
    status_text: String,
    headers: HashMap<String, String>,
    mime_type: String,
    request_id: String,
    session: Option<Arc<dyn Session>>,
}

impl ResponseData {
    pub(crate) fn from_cdp(
        r: &network::Response,
        request_id: &str,
        session: Arc<dyn Session>,
    ) -> Arc<Self> {
        Arc::new(Self {
            url: r.url.clone(),
            status: r.status as u16,
            status_text: r.status_text.clone(),
            headers: header_map(&r.headers),
            mime_type: r.mime_type.clone(),
            request_id: request_id.to_owned(),
            session: Some(session),
        })
    }
}

/// An HTTP response. Cheap to clone.
#[derive(Clone)]
pub struct Response {
    data: Arc<ResponseData>,
    request: Option<Request>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Response({} {})", self.data.status, self.data.url)
    }
}

impl Response {
    pub(crate) fn new(data: Arc<ResponseData>, request: Option<Request>) -> Self {
        Self { data, request }
    }

    /// A response not tied to a browser request, for tests of code that consumes responses.
    pub fn synthetic(
        url: &str,
        status: u16,
        status_text: &str,
        headers: HashMap<String, String>,
    ) -> Self {
        Self {
            data: Arc::new(ResponseData {
                url: url.to_owned(),
                status,
                status_text: status_text.to_owned(),
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v))
                    .collect(),
                mime_type: String::new(),
                request_id: String::new(),
                session: None,
            }),
            request: None,
        }
    }

    /// The response's URL.
    pub fn url(&self) -> &str {
        &self.data.url
    }

    /// The HTTP status code.
    pub fn status(&self) -> u16 {
        self.data.status
    }

    /// The HTTP status text, such as `OK`.
    pub fn status_text(&self) -> &str {
        &self.data.status_text
    }

    /// Whether the status is 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.data.status)
    }

    /// Response headers, with lowercase names.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.data.headers
    }

    /// One header, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.data
            .headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The MIME type, such as `text/html`.
    pub fn mime_type(&self) -> &str {
        &self.data.mime_type
    }

    /// The request this answers.
    pub fn request(&self) -> Option<&Request> {
        self.request.as_ref()
    }

    /// The body, once it has finished loading.
    pub async fn body(&self) -> Result<Vec<u8>> {
        let session = self
            .data
            .session
            .as_ref()
            .ok_or_else(|| Error::invalid_argument("this response has no body to read"))?;
        if let Some(request) = &self.request {
            request.settled(BODY_TIMEOUT).await?;
        }
        let body = session
            .send(&network::GetResponseBodyParams {
                request_id: self.data.request_id.clone(),
            })
            .await?;
        if body.base64_encoded {
            base64::engine::general_purpose::STANDARD
                .decode(body.body)
                .map_err(|e| Error::other(format!("the browser sent invalid base64: {e}")))
        } else {
            Ok(body.body.into_bytes())
        }
    }

    /// The body as UTF-8 text.
    pub async fn text(&self) -> Result<String> {
        String::from_utf8(self.body().await?)
            .map_err(|e| Error::other(format!("response body is not UTF-8: {e}")))
    }

    /// The body parsed as JSON.
    pub async fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body().await?)
            .map_err(|e| Error::other(format!("response body is not the expected JSON: {e}")))
    }
}

/// How to answer an intercepted request without the network.
#[derive(Debug, Clone, Default)]
pub struct FulfillOptions {
    /// Defaults to 200.
    pub status: Option<u16>,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// The `Content-Type` header.
    pub content_type: Option<String>,
    /// The response body.
    pub body: Vec<u8>,
    /// Read the body from this file instead.
    pub path: Option<PathBuf>,
}

impl FulfillOptions {
    /// A 200 response with a text body.
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            content_type: Some("text/plain; charset=utf-8".into()),
            body: body.into().into_bytes(),
            ..Self::default()
        }
    }

    /// A 200 response with an HTML body.
    pub fn html(body: impl Into<String>) -> Self {
        Self {
            content_type: Some("text/html; charset=utf-8".into()),
            body: body.into().into_bytes(),
            ..Self::default()
        }
    }

    /// A 200 response with `value` as its JSON body.
    pub fn json<T: Serialize>(value: &T) -> Self {
        Self {
            content_type: Some("application/json".into()),
            body: serde_json::to_vec(value).unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Sets the status.
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Adds a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(name.into(), value.into());
        self
    }
}

/// Changes to make to an intercepted request before letting it through.
#[derive(Debug, Clone, Default)]
pub struct ContinueOptions {
    /// Send the request to this URL instead.
    pub url: Option<String>,
    /// Use this HTTP method instead.
    pub method: Option<String>,
    /// Replaces all headers.
    pub headers: Option<HashMap<String, String>>,
    /// Send this body instead.
    pub post_data: Option<String>,
}

/// An intercepted request, waiting to be fulfilled, continued or aborted. A handler that does
/// none of these lets the request continue unchanged.
pub struct Route {
    request: Request,
    session: Arc<dyn Session>,
    fetch_id: String,
    handled: AtomicBool,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Route({:?})", self.request)
    }
}

impl Route {
    /// The request being intercepted.
    pub fn request(&self) -> &Request {
        &self.request
    }

    fn take(&self) -> Result<()> {
        if self.handled.swap(true, Ordering::SeqCst) {
            return Err(Error::invalid_argument("Route has already been handled."));
        }
        Ok(())
    }

    /// Answers the request with a made-up response.
    pub async fn fulfill(&self, options: FulfillOptions) -> Result<()> {
        self.take()?;
        let body = match &options.path {
            Some(path) => std::fs::read(path)
                .map_err(|e| Error::io(format!("reading {}", path.display()), e))?,
            None => options.body,
        };
        let mut headers: Vec<fetch::HeaderEntry> = options
            .headers
            .iter()
            .filter(|(k, _)| {
                options.content_type.is_none() || !k.eq_ignore_ascii_case("content-type")
            })
            .map(|(name, value)| fetch::HeaderEntry {
                name: name.clone(),
                value: value.clone(),
            })
            .collect();
        if let Some(ct) = options.content_type {
            headers.push(fetch::HeaderEntry {
                name: "Content-Type".into(),
                value: ct,
            });
        }
        self.session
            .send(&fetch::FulfillRequestParams {
                request_id: self.fetch_id.clone(),
                response_code: i64::from(options.status.unwrap_or(200)),
                response_headers: Some(headers),
                body: Some(base64::engine::general_purpose::STANDARD.encode(body)),
                binary_response_headers: None,
                response_phrase: None,
            })
            .await?;
        Ok(())
    }

    /// Lets the request through, optionally changed.
    pub async fn continue_(&self, options: ContinueOptions) -> Result<()> {
        self.take()?;
        continue_request(&self.session, &self.fetch_id, options).await
    }

    /// Fails the request. `error_code` is one of `failed` (the default), `aborted`,
    /// `timedout`, `accessdenied`, `connectionrefused`, `connectionreset`,
    /// `connectionfailed`, `namenotresolved`, `internetdisconnected`, `addressunreachable`,
    /// `blockedbyclient` or `blockedbyresponse`.
    pub async fn abort(&self, error_code: Option<&str>) -> Result<()> {
        self.take()?;
        let reason = match error_code.unwrap_or("failed").to_ascii_lowercase().as_str() {
            "aborted" => "Aborted",
            "accessdenied" => "AccessDenied",
            "connectionrefused" => "ConnectionRefused",
            "connectionreset" => "ConnectionReset",
            "timedout" => "TimedOut",
            "connectionfailed" => "ConnectionFailed",
            "namenotresolved" => "NameNotResolved",
            "internetdisconnected" => "InternetDisconnected",
            "addressunreachable" => "AddressUnreachable",
            "blockedbyclient" => "BlockedByClient",
            "blockedbyresponse" => "BlockedByResponse",
            _ => "Failed",
        };
        let error_reason =
            serde_json::from_value(json!(reason)).expect("known error reasons decode");
        self.session
            .send(&fetch::FailRequestParams {
                request_id: self.fetch_id.clone(),
                error_reason,
            })
            .await?;
        Ok(())
    }
}

async fn continue_request(
    session: &Arc<dyn Session>,
    fetch_id: &str,
    options: ContinueOptions,
) -> Result<()> {
    session
        .send(&fetch::ContinueRequestParams {
            request_id: fetch_id.to_owned(),
            url: options.url,
            method: options.method,
            post_data: options
                .post_data
                .map(|p| base64::engine::general_purpose::STANDARD.encode(p)),
            headers: options.headers.map(|h| {
                h.into_iter()
                    .map(|(name, value)| fetch::HeaderEntry { name, value })
                    .collect()
            }),
            intercept_response: None,
        })
        .await?;
    Ok(())
}

type RouteFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type RouteHandlerFn = Arc<dyn Fn(Arc<Route>) -> RouteFuture + Send + Sync>;

/// Which URLs a route intercepts: a glob (`**/api/**`), a regex, a predicate, or a plain
/// string matching any URL that contains it.
#[derive(Clone)]
pub struct RoutePattern(UrlMatcher);

impl RoutePattern {
    /// Whether `url` matches the pattern.
    pub fn matches(&self, url: &str) -> bool {
        match &self.0 {
            UrlMatcher::Glob(p) if !p.contains('*') => url == p || url.contains(p.as_str()),
            m => m.matches(url),
        }
    }
}

impl<T: Into<UrlMatcher>> From<T> for RoutePattern {
    fn from(m: T) -> Self {
        RoutePattern(m.into())
    }
}

/// A registered route: what it matches and how it answers.
#[derive(Clone)]
pub(crate) struct RouteEntry {
    pub id: u64,
    pub pattern: RoutePattern,
    pub handler: RouteHandlerFn,
}

/// Identifies a route for [`crate::Page::unroute`] and [`crate::BrowserContext::unroute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteId(pub(crate) u64);

pub(crate) fn make_route_entry<F, Fut>(pattern: RoutePattern, handler: F) -> RouteEntry
where
    F: Fn(Arc<Route>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    RouteEntry {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        pattern,
        handler: Arc::new(move |route| Box::pin(handler(route))),
    }
}

/// The last-registered route matching `url` wins, page routes before context routes.
pub(crate) fn find_route(
    page_routes: &[RouteEntry],
    context_routes: &[RouteEntry],
    url: &str,
) -> Option<RouteEntry> {
    page_routes
        .iter()
        .rev()
        .chain(context_routes.iter().rev())
        .find(|r| r.pattern.matches(url))
        .cloned()
}

/// Handles one `Fetch.requestPaused`: runs the matching route handler, or lets the request
/// through. A handler that does not settle the route, or panics, lets it through too.
pub(crate) async fn handle_paused(
    page: Weak<PageInner>,
    session: Arc<dyn Session>,
    event: fetch::RequestPausedEvent,
    route: Option<RouteEntry>,
) {
    let Some(entry) = route else {
        let _ = continue_request(&session, &event.request_id, ContinueOptions::default()).await;
        return;
    };
    let request = Request::from_cdp(
        event
            .network_id
            .clone()
            .unwrap_or_else(|| event.request_id.clone()),
        &event.request,
        Some(event.resource_type),
        Some(event.frame_id.clone()),
        page,
    );
    let route = Arc::new(Route {
        request,
        session: session.clone(),
        fetch_id: event.request_id.clone(),
        handled: AtomicBool::new(false),
    });
    let outcome = tokio::spawn((entry.handler)(route.clone())).await;
    if let Err(e) = &outcome {
        tracing::warn!(url = %event.request.url, error = %e, "route handler panicked; continuing the request");
    }
    if !route.handled.swap(true, Ordering::SeqCst) {
        let _ = continue_request(&session, &event.request_id, ContinueOptions::default()).await;
    }
}

/// A network event, for handlers and waiters.
#[derive(Debug, Clone)]
pub(crate) enum NetworkEvent {
    Request(Request),
    Response(Response),
    RequestFinished(Request),
    RequestFailed(Request),
}

/// Per-page network bookkeeping.
#[derive(Default)]
pub(crate) struct NetworkState {
    /// Requests by id, while they are in flight.
    pub requests: HashMap<String, Request>,
    pub routes: Vec<RouteEntry>,
    /// Sessions (by id; `""` for the page's own) with `Fetch` interception on.
    pub fetch_sessions: HashSet<String>,
    pub har: Option<HarRecorder>,
}

/// Collects a HAR 1.2 log of what the page loaded.
#[derive(Default)]
pub(crate) struct HarRecorder {
    pending: HashMap<String, HarEntryBuilder>,
    entries: Vec<Value>,
}

#[derive(Default)]
struct HarEntryBuilder {
    method: String,
    url: String,
    request_headers: Value,
    post_data: Option<String>,
    wall_time: f64,
    start: f64,
    response_at: Option<f64>,
    status: i64,
    status_text: String,
    response_headers: Value,
    mime_type: String,
}

fn har_headers(headers: &Value) -> Value {
    Value::Array(
        headers
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| json!({ "name": k, "value": v.as_str().unwrap_or_default() }))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn har_query(url: &str) -> Value {
    let query = url
        .split_once('?')
        .map(|(_, q)| q.split('#').next().unwrap_or(q))
        .unwrap_or("");
    Value::Array(
        query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                json!({ "name": k, "value": v })
            })
            .collect(),
    )
}

impl HarRecorder {
    pub fn on_request(&mut self, e: &network::RequestWillBeSentEvent) {
        self.pending.insert(
            e.request_id.clone(),
            HarEntryBuilder {
                method: e.request.method.clone(),
                url: e.request.url.clone(),
                request_headers: e.request.headers.clone(),
                post_data: e.request.post_data.clone(),
                wall_time: e.wall_time,
                start: e.timestamp,
                ..Default::default()
            },
        );
    }

    pub fn on_response(&mut self, e: &network::ResponseReceivedEvent) {
        if let Some(b) = self.pending.get_mut(&e.request_id) {
            b.status = e.response.status;
            b.status_text = e.response.status_text.clone();
            b.response_headers = e.response.headers.clone();
            b.mime_type = e.response.mime_type.clone();
            b.response_at = Some(e.timestamp);
        }
    }

    pub fn on_end(&mut self, request_id: &str, timestamp: f64, size: f64, error: Option<&str>) {
        let Some(b) = self.pending.remove(request_id) else {
            return;
        };
        let total = (timestamp - b.start).max(0.0) * 1000.0;
        let wait = (b.response_at.unwrap_or(b.start) - b.start).max(0.0) * 1000.0;
        let started = chrono_like_iso(b.wall_time);
        self.entries.push(json!({
            "startedDateTime": started,
            "time": total,
            "request": {
                "method": b.method,
                "url": b.url,
                "httpVersion": "HTTP/1.1",
                "headers": har_headers(&b.request_headers),
                "queryString": har_query(&b.url),
                "cookies": [],
                "headersSize": -1,
                "bodySize": b.post_data.as_ref().map_or(-1, |p| p.len() as i64),
                "postData": b.post_data.as_ref().map(|p| json!({ "mimeType": "application/x-www-form-urlencoded", "text": p })),
            },
            "response": {
                "status": if error.is_some() { 0 } else { b.status },
                "statusText": error.map(str::to_owned).unwrap_or(b.status_text),
                "httpVersion": "HTTP/1.1",
                "headers": har_headers(&b.response_headers),
                "cookies": [],
                "content": { "size": size as i64, "mimeType": if b.mime_type.is_empty() { "application/octet-stream".to_owned() } else { b.mime_type } },
                "redirectURL": "",
                "headersSize": -1,
                "bodySize": size as i64,
            },
            "cache": {},
            "timings": { "send": 0, "wait": wait, "receive": (total - wait).max(0.0) },
        }));
    }

    pub fn to_har(&self) -> Value {
        json!({
            "log": {
                "version": "1.2",
                "creator": { "name": "agito", "version": env!("CARGO_PKG_VERSION") },
                "pages": [],
                "entries": self.entries,
            }
        })
    }
}

/// Seconds since the epoch as an ISO 8601 UTC timestamp with milliseconds.
fn chrono_like_iso(seconds: f64) -> String {
    let millis = (seconds * 1000.0).round() as i64;
    let (days, ms_of_day) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (h, rem) = (ms_of_day / 3_600_000, ms_of_day % 3_600_000);
    let (min, rem) = (rem / 60_000, rem % 60_000);
    let (s, ms) = (rem / 1000, rem % 1000);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{ms:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_patterns() {
        let p: RoutePattern = "/api/users".into();
        assert!(p.matches("http://h/api/users?x=1"));
        assert!(!p.matches("http://h/api/orders"));
        let p: RoutePattern = "**/*.png".into();
        assert!(p.matches("http://h/a/b.png"));
        assert!(!p.matches("http://h/a/b.jpg"));
        let p: RoutePattern = regex::Regex::new(r"/items/\d+$").unwrap().into();
        assert!(p.matches("http://h/items/42"));
    }

    #[test]
    fn last_route_wins_and_page_beats_context() {
        let a = make_route_entry("**/a".into(), |_r| async {});
        let b = make_route_entry("**/a".into(), |_r| async {});
        let c = make_route_entry("**".into(), |_r| async {});
        assert_eq!(
            find_route(
                &[a.clone(), b.clone()],
                std::slice::from_ref(&c),
                "http://h/a"
            )
            .unwrap()
            .id,
            b.id
        );
        assert_eq!(
            find_route(&[a], std::slice::from_ref(&c), "http://h/z")
                .unwrap()
                .id,
            c.id
        );
        assert!(find_route(&[], &[], "http://h/z").is_none());
    }

    #[test]
    fn iso_timestamps() {
        assert_eq!(chrono_like_iso(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            chrono_like_iso(1_700_000_000.123),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(chrono_like_iso(951_782_400.0), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn har_query_strings() {
        assert_eq!(
            har_query("http://h/p?a=1&b=&c#f"),
            json!([
                { "name": "a", "value": "1" }, { "name": "b", "value": "" }, { "name": "c", "value": "" }
            ])
        );
    }

    #[test]
    fn fulfill_option_builders() {
        let f = FulfillOptions::json(&json!({ "ok": true }))
            .with_status(201)
            .with_header("X-A", "1");
        assert_eq!(f.status, Some(201));
        assert_eq!(f.content_type.as_deref(), Some("application/json"));
        assert_eq!(f.body, br#"{"ok":true}"#);
        assert_eq!(f.headers["X-A"], "1");
    }
}
