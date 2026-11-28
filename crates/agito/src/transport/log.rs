//! The protocol log: every message agito sends to the browser and every message it gets back,
//! one line each, for working out whether a problem is in agito, the page or the browser.
//!
//! Turn it on with `AGITO_PROTOCOL_LOG=target/protocol.log` (or `stderr`), with
//! `launch.protocolLog` in `agito.config.json`, or in code with
//! [`LaunchOptions::protocol_log`](crate::LaunchOptions::protocol_log). Each line is a UTC
//! timestamp, a connection tag such as `c1`, then one of:
//!
//! ```text
//! -> #42 <session> Method {params}          a command sent
//! <- #42 <session> Method 2.1ms {result}    its answer, or `error <code> "message"`
//! <- event <session> Method {params}         an event
//! xx #42 <session> Method cancelled after 60000.0ms
//! open <kind> <what>, closed (<reason>), <- malformed <text>
//! ```
//!
//! The session is `-` for the browser itself. Long strings are trimmed, and secrets are
//! replaced: cookie values, authorization, cookie and token-like headers, raw header text,
//! password fields and request bodies. Typed text, filled values and script arguments are kept,
//! since hiding them would hide most of what the log is for: treat a log file as sensitive.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// The variable that turns the log on without a code change: a file path, or `stderr`.
pub const ENV_VAR: &str = "AGITO_PROTOCOL_LOG";

/// A callback that receives each log line.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Where and how to write the protocol log. Off by default.
#[derive(Clone)]
pub struct ProtocolLogOptions {
    /// A file to append to, or `stderr`. The file and its directory are created as needed; on
    /// Unix a new file is readable by its owner only. Several connections, and several
    /// processes, can share one file: each line names its connection, and each process starts
    /// its part with a header.
    pub path: Option<PathBuf>,
    /// Receives each line, without a newline, on a background thread.
    pub sink: Option<LogSink>,
    /// The longest a string value may be before it is trimmed, in characters; 0 for no limit.
    pub max_value_length: usize,
    /// Replace secrets before a line is written. On by default; there is no variable for it,
    /// so logging secrets takes a deliberate change to code.
    pub redact_secrets: bool,
}

impl Default for ProtocolLogOptions {
    fn default() -> Self {
        ProtocolLogOptions {
            path: None,
            sink: None,
            max_value_length: 1024,
            redact_secrets: true,
        }
    }
}

impl std::fmt::Debug for ProtocolLogOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolLogOptions")
            .field("path", &self.path)
            .field("sink", &self.sink.as_ref().map(|_| "Fn(&str)"))
            .field("max_value_length", &self.max_value_length)
            .field("redact_secrets", &self.redact_secrets)
            .finish()
    }
}

impl ProtocolLogOptions {
    /// Options that log to `path` (a file, or `stderr`).
    pub fn to(path: impl Into<PathBuf>) -> Self {
        ProtocolLogOptions {
            path: Some(path.into()),
            ..Default::default()
        }
    }

    /// The options to log with, or `None` when the log is off: `options` when they name a
    /// destination, otherwise `fallback_path` (from configuration) with the rest of `options`.
    /// `stdout` is refused, since it carries test and runner output.
    pub(crate) fn resolve(
        options: Option<&ProtocolLogOptions>,
        fallback_path: Option<&str>,
    ) -> Option<ProtocolLogOptions> {
        let mut resolved = options.cloned().unwrap_or_default();
        if resolved.path.is_none() && resolved.sink.is_none() {
            resolved.path = fallback_path
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from);
        }
        if resolved
            .path
            .as_deref()
            .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case("stdout"))
        {
            eprintln!(
                "agito: the protocol log cannot go to stdout, which carries test output; \
                 set {ENV_VAR} to a file path or to stderr"
            );
            resolved.path = None;
        }
        (resolved.path.is_some() || resolved.sink.is_some()).then_some(resolved)
    }
}

/// The log of one connection.
pub(crate) struct ProtocolLog {
    tag: String,
    sinks: Vec<Arc<Sink>>,
    max_value_length: usize,
    redact: bool,
    /// Commands sent and not yet answered, by id.
    commands: Mutex<HashMap<u64, Sent>>,
    closed: AtomicBool,
}

/// A command waiting for its answer: when it went, which method, on which session.
struct Sent {
    at: Instant,
    method: String,
    session: Option<String>,
}

static NEXT_CONNECTION: AtomicU64 = AtomicU64::new(1);

impl ProtocolLog {
    /// Opens a log for one connection. `kind` is `cdp-pipe` or `cdp-ws`, and `what` describes
    /// the other end, without anything that would grant control of the browser.
    pub(crate) fn open(options: &ProtocolLogOptions, kind: &str, what: &str) -> Option<Arc<Self>> {
        let mut sinks = Vec::new();
        if let Some(path) = &options.path {
            match Sink::shared(path) {
                Ok(sink) => sinks.push(sink),
                Err(e) => eprintln!(
                    "agito: the protocol log could not open {}: {e}",
                    path.display()
                ),
            }
        }
        if let Some(callback) = &options.sink {
            sinks.push(Sink::callback(callback.clone()));
        }
        if sinks.is_empty() {
            return None;
        }
        let log = Arc::new(ProtocolLog {
            tag: format!("c{}", NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed)),
            sinks,
            max_value_length: options.max_value_length,
            redact: options.redact_secrets,
            commands: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        log.write(&format!("open {kind} {what}"));
        Some(log)
    }

    fn write(&self, entry: &str) {
        let line = format!("{} {} {entry}", timestamp(SystemTime::now()), self.tag);
        for sink in &self.sinks {
            sink.send(line.clone());
        }
    }

    fn payload(&self, value: Option<&Value>, method: Option<&str>) -> String {
        match value {
            Some(v) => format_payload(v, method, self.max_value_length, self.redact),
            None => "{}".to_owned(),
        }
    }

    /// A command, logged as it goes out.
    pub(crate) fn command(&self, id: u64, session: Option<&str>, method: &str, params: &Value) {
        self.commands.lock().unwrap().insert(
            id,
            Sent {
                at: Instant::now(),
                method: method.to_owned(),
                session: session.map(str::to_owned),
            },
        );
        self.write(&format!(
            "-> #{id} {} {method} {}",
            session.unwrap_or("-"),
            self.payload(Some(params), Some(method))
        ));
    }

    /// An answer to a command: its result, or an error.
    pub(crate) fn response(&self, id: u64, result: Option<&Value>, error: Option<(i64, &str)>) {
        let started = self.commands.lock().unwrap().remove(&id);
        let (took, method, session) = match &started {
            Some(sent) => (
                format!("{:.1}ms", sent.at.elapsed().as_secs_f64() * 1000.0),
                sent.method.as_str(),
                sent.session.as_deref(),
            ),
            None => ("?ms".to_owned(), "?", None),
        };
        let body = match error {
            Some((code, message)) => format!(
                "error {code} {}",
                Value::from(trim(message, self.max_value_length))
            ),
            None => self.payload(result, Some(method)),
        };
        self.write(&format!(
            "<- #{id} {} {method} {took} {body}",
            session.unwrap_or("-")
        ));
    }

    /// An event.
    pub(crate) fn event(&self, session: Option<&str>, method: &str, params: Option<&Value>) {
        self.write(&format!(
            "<- event {} {method} {}",
            session.unwrap_or("-"),
            self.payload(params, Some(method))
        ));
    }

    /// A command that ended without an answer: timed out, cancelled, or cut off by the
    /// connection closing. Does nothing if its answer was logged.
    pub(crate) fn cancelled(&self, id: u64) {
        let Some(sent) = self.commands.lock().unwrap().remove(&id) else {
            return;
        };
        self.write(&format!(
            "xx #{id} {} {} cancelled after {:.1}ms",
            sent.session.as_deref().unwrap_or("-"),
            sent.method,
            sent.at.elapsed().as_secs_f64() * 1000.0
        ));
    }

    /// A message that was not valid JSON, which the connection drops.
    pub(crate) fn malformed(&self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let text = trim(&text, self.max_value_length)
            .replace('\r', "\\r")
            .replace('\n', "\\n");
        self.write(&format!("<- malformed {text}"));
    }

    /// The end of the connection, logged once, after which everything logged so far is
    /// written out.
    pub(crate) fn closed(&self, reason: Option<&str>) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let pending: Vec<u64> = self.commands.lock().unwrap().keys().copied().collect();
        for id in pending {
            self.cancelled(id);
        }
        match reason {
            Some(reason) => self.write(&format!("closed ({reason})")),
            None => self.write("closed"),
        }
        for sink in &self.sinks {
            sink.flush();
        }
    }
}

/// `ws://host:port/devtools/browser/<id>` as `host:port`: the path is what lets anyone drive the
/// browser, so it stays out of the log.
pub(crate) fn describe_endpoint(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split('/').next().unwrap_or(rest).to_owned()
}

enum Message {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

/// A destination with a writer thread of its own, so neither sending nor receiving ever waits
/// on a disk or a caller's code.
struct Sink {
    queue: mpsc::Sender<Message>,
}

/// File and stderr sinks, shared by every connection in the process that logs to them.
static SHARED: LazyLock<Mutex<HashMap<PathBuf, Weak<Sink>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl Sink {
    fn shared(path: &Path) -> std::io::Result<Arc<Sink>> {
        let stderr = path.as_os_str().eq_ignore_ascii_case("stderr");
        let key = if stderr {
            PathBuf::from("stderr")
        } else {
            std::path::absolute(path)?
        };
        let mut shared = SHARED.lock().unwrap();
        if let Some(sink) = shared.get(&key).and_then(Weak::upgrade) {
            return Ok(sink);
        }
        let mut out: Box<dyn std::io::Write + Send> = if stderr {
            Box::new(std::io::stderr())
        } else {
            Box::new(open_append(&key)?)
        };
        // Each process starts its part of the file with a header.
        let header = format!(
            "{} -- agito {} protocol log, process {}\n",
            timestamp(SystemTime::now()),
            env!("CARGO_PKG_VERSION"),
            std::process::id()
        );
        out.write_all(header.as_bytes())?;
        let sink = Arc::new(Sink::spawn(move |batch: &str| {
            let _ = out.write_all(batch.as_bytes());
            let _ = out.flush();
        }));
        shared.insert(key, Arc::downgrade(&sink));
        Ok(sink)
    }

    fn callback(callback: LogSink) -> Arc<Sink> {
        Arc::new(Sink::spawn(move |batch: &str| {
            for line in batch.lines() {
                // A failing callback must not stop the log.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(line)));
            }
        }))
    }

    /// Starts the writer. It takes whatever is queued and writes it in one go, so a file gets
    /// each batch as a single append and stays current to within moments.
    fn spawn(mut write: impl FnMut(&str) + Send + 'static) -> Sink {
        let (queue, rx) = mpsc::channel::<Message>();
        std::thread::Builder::new()
            .name("agito-protocol-log".into())
            .spawn(move || {
                let mut batch = String::new();
                while let Ok(first) = rx.recv() {
                    let mut acks = Vec::new();
                    let mut next = Some(first);
                    while let Some(message) = next {
                        match message {
                            Message::Line(line) => {
                                batch.push_str(&line);
                                batch.push('\n');
                            }
                            Message::Flush(ack) => acks.push(ack),
                        }
                        next = rx.try_recv().ok();
                    }
                    if !batch.is_empty() {
                        write(&batch);
                        batch.clear();
                    }
                    for ack in acks {
                        let _ = ack.send(());
                    }
                }
            })
            .expect("the protocol log's writer thread starts");
        Sink { queue }
    }

    fn send(&self, line: String) {
        let _ = self.queue.send(Message::Line(line));
    }

    /// Waits, briefly, for everything queued to be written.
    fn flush(&self) {
        let (ack, done) = mpsc::sync_channel(1);
        if self.queue.send(Message::Flush(ack)).is_ok() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }
}

/// Opens `path` for appending. Every write goes to the end of the file, whoever else is
/// writing to it, so processes sharing a log never overwrite each other's lines.
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The longest a formatted payload may be, in bytes. Past it the line is cut and no longer
/// valid JSON; a DOM snapshot or a coverage report can reach it.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

const REDACTED: &str = "<redacted>";

/// Formats a payload as compact JSON for one line, trimming long strings and, with `redact`,
/// replacing secrets. Redaction goes by property name and applies in both directions, because
/// the browser sends cookies and authorization headers in events nobody asked for.
pub(crate) fn format_payload(
    value: &Value,
    method: Option<&str>,
    max_value_length: usize,
    redact: bool,
) -> String {
    let scope = if redact && method == Some("Network.setCookie") {
        Scope::Cookie
    } else {
        Scope::Plain
    };
    let cleaned = clean(value, scope, max_value_length, redact);
    let mut text = serde_json::to_string(&cleaned).expect("JSON values serialize");
    if text.len() > MAX_PAYLOAD_BYTES {
        let mut cut = MAX_PAYLOAD_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let dropped = text.len() - cut;
        text.truncate(cut);
        let _ = write!(text, "…[+{dropped} bytes]");
    }
    text
}

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Plain,
    /// Inside a cookie or a list of cookies, where `value` is the secret.
    Cookie,
}

fn clean(value: &Value, scope: Scope, max: usize, redact: bool) -> Value {
    match value {
        Value::String(s) => Value::String(trim(s, max)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| clean(v, scope, max, redact)).collect())
        }
        Value::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (name, v) in map {
                let cleaned = if !redact {
                    clean(v, Scope::Plain, max, redact)
                } else if name.eq_ignore_ascii_case("password")
                    || matches!(
                        name.as_str(),
                        "headersText"
                            | "requestHeadersText"
                            | "binaryResponseHeaders"
                            | "cookieLine"
                    )
                    || (scope == Scope::Cookie && name == "value")
                {
                    Value::from(REDACTED)
                } else if matches!(name.as_str(), "postData" | "postDataEntries") {
                    Value::from(describe_body(v))
                } else if matches!(
                    name.as_str(),
                    "headers" | "requestHeaders" | "responseHeaders" | "extraHTTPHeaders"
                ) {
                    clean_headers(v, max)
                } else if matches!(
                    name.as_str(),
                    "cookies"
                        | "cookie"
                        | "associatedCookies"
                        | "blockedCookies"
                        | "exemptedCookies"
                ) {
                    clean(v, Scope::Cookie, max, redact)
                } else {
                    clean(v, scope, max, redact)
                };
                out.insert(name.clone(), cleaned);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Headers with the values of sensitive ones replaced: CDP keys them by name, and Fetch lists
/// them as `{name, value}` pairs.
fn clean_headers(headers: &Value, max: usize) -> Value {
    match headers {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, v)| {
                    let v = if is_sensitive_header(name) {
                        Value::from(REDACTED)
                    } else {
                        clean(v, Scope::Plain, max, true)
                    };
                    (name.clone(), v)
                })
                .collect(),
        ),
        Value::Array(entries) => Value::Array(
            entries
                .iter()
                .map(|entry| {
                    let sensitive = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(is_sensitive_header);
                    match entry {
                        Value::Object(fields) if sensitive => Value::Object(
                            fields
                                .iter()
                                .map(|(k, v)| {
                                    let v = if k == "value" {
                                        Value::from(REDACTED)
                                    } else {
                                        clean(v, Scope::Plain, max, true)
                                    };
                                    (k.clone(), v)
                                })
                                .collect(),
                        ),
                        other => clean(other, Scope::Plain, max, true),
                    }
                })
                .collect(),
        ),
        other => clean(other, Scope::Plain, max, true),
    }
}

/// Whether a header's value is a credential: the well-known names, and anything that looks
/// like it carries a token or a key.
pub(crate) fn is_sensitive_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "cookie"
        || name == "set-cookie"
        || ["auth", "token", "secret", "api-key", "apikey"]
            .iter()
            .any(|part| name.contains(part))
}

fn describe_body(value: &Value) -> String {
    match value {
        Value::String(s) => format!("<redacted {} chars>", s.chars().count()),
        Value::Array(entries) => format!("<redacted {} entries>", entries.len()),
        _ => REDACTED.to_owned(),
    }
}

/// `value` cut to `max` characters, saying how many went; `max` of 0 keeps it whole.
pub(crate) fn trim(value: &str, max: usize) -> String {
    if max == 0 {
        return value.to_owned();
    }
    let total = value.chars().count();
    if total <= max {
        return value.to_owned();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…[+{} chars]", total - max)
}

/// `2026-11-27T15:04:05.123Z`.
pub(crate) fn timestamp(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
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
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fmt(value: Value, method: Option<&str>) -> Value {
        serde_json::from_str(&format_payload(&value, method, 1024, true)).unwrap()
    }

    #[test]
    fn headers_are_redacted_by_name_in_either_shape() {
        let out = fmt(
            json!({ "request": { "headers": {
                "Authorization": "Bearer abc", "Cookie": "sid=1", "X-Api-Key": "k",
                "X-Auth-Token": "t", "Accept": "text/html" } } }),
            None,
        );
        let h = &out["request"]["headers"];
        for name in ["Authorization", "Cookie", "X-Api-Key", "X-Auth-Token"] {
            assert_eq!(h[name], REDACTED, "{name}");
        }
        assert_eq!(h["Accept"], "text/html");

        let out = fmt(
            json!({ "headers": [
                { "name": "set-cookie", "value": "sid=2" },
                { "name": "content-type", "value": "text/plain" } ] }),
            Some("Fetch.fulfillRequest"),
        );
        assert_eq!(out["headers"][0]["value"], REDACTED);
        assert_eq!(out["headers"][1]["value"], "text/plain");
    }

    #[test]
    fn cookies_passwords_bodies_and_raw_headers_are_redacted() {
        let out = fmt(
            json!({ "cookies": [{ "name": "sid", "value": "secret", "domain": "a.test" }],
                    "form": { "Password": "hunter2", "user": "ada" },
                    "postData": "user=ada&pass=x",
                    "postDataEntries": [{ "bytes": "a" }, { "bytes": "b" }],
                    "headersText": "GET / HTTP/1.1\r\nCookie: sid=1" }),
            None,
        );
        assert_eq!(out["cookies"][0]["value"], REDACTED);
        assert_eq!(out["cookies"][0]["name"], "sid");
        assert_eq!(out["form"]["Password"], REDACTED);
        assert_eq!(out["form"]["user"], "ada");
        assert_eq!(out["postData"], "<redacted 15 chars>");
        assert_eq!(out["postDataEntries"], "<redacted 2 entries>");
        assert_eq!(out["headersText"], REDACTED);

        // Network.setCookie has its cookie at the root of the params.
        let out = fmt(
            json!({ "name": "sid", "value": "secret" }),
            Some("Network.setCookie"),
        );
        assert_eq!(out["value"], REDACTED);
        // Elsewhere a `value` is not a secret.
        let out = fmt(json!({ "value": "typed text" }), Some("Input.insertText"));
        assert_eq!(out["value"], "typed text");
    }

    #[test]
    fn redaction_can_be_turned_off() {
        let text = format_payload(&json!({ "password": "x" }), None, 0, false);
        assert_eq!(text, r#"{"password":"x"}"#);
    }

    #[test]
    fn long_strings_and_payloads_are_trimmed() {
        assert_eq!(trim("abcdef", 4), "abcd…[+2 chars]");
        assert_eq!(trim("abc", 4), "abc");
        assert_eq!(trim("ééééé", 2), "éé…[+3 chars]");
        assert_eq!(trim("abcdef", 0), "abcdef");
        let out = fmt(json!({ "data": "x".repeat(2000) }), None);
        assert!(out["data"].as_str().unwrap().ends_with("…[+976 chars]"));

        let huge = json!({ "items": (0..20_000).map(|i| format!("item {i}")).collect::<Vec<_>>() });
        let text = format_payload(&huge, None, 1024, true);
        assert!(text.len() < MAX_PAYLOAD_BYTES + 32);
        assert!(text.contains("…[+"), "says how much was cut");
    }

    #[test]
    fn sensitive_header_names() {
        for name in [
            "Cookie",
            "set-cookie",
            "authorization",
            "Proxy-Authorization",
            "x-csrf-token",
            "X-API-KEY",
            "client-secret",
        ] {
            assert!(is_sensitive_header(name), "{name}");
        }
        for name in ["accept", "content-type", "user-agent", "x-request-id"] {
            assert!(!is_sensitive_header(name), "{name}");
        }
    }

    #[test]
    fn options_resolve_code_first_then_configuration() {
        assert!(ProtocolLogOptions::resolve(None, None).is_none());
        assert!(ProtocolLogOptions::resolve(None, Some("  ")).is_none());
        let from_env = ProtocolLogOptions::resolve(None, Some("target/p.log")).unwrap();
        assert_eq!(from_env.path.as_deref(), Some(Path::new("target/p.log")));

        let code = ProtocolLogOptions {
            max_value_length: 10,
            ..ProtocolLogOptions::to("mine.log")
        };
        let resolved = ProtocolLogOptions::resolve(Some(&code), Some("env.log")).unwrap();
        assert_eq!(resolved.path.as_deref(), Some(Path::new("mine.log")));

        // Settings from code, destination from the environment.
        let limits = ProtocolLogOptions {
            max_value_length: 10,
            ..Default::default()
        };
        let resolved = ProtocolLogOptions::resolve(Some(&limits), Some("env.log")).unwrap();
        assert_eq!(resolved.path.as_deref(), Some(Path::new("env.log")));
        assert_eq!(resolved.max_value_length, 10);

        assert!(ProtocolLogOptions::resolve(None, Some("stdout")).is_none());
    }

    #[test]
    fn endpoints_keep_host_and_port_only() {
        assert_eq!(
            describe_endpoint("ws://127.0.0.1:9222/devtools/browser/0b1c-secret"),
            "127.0.0.1:9222"
        );
        assert_eq!(describe_endpoint("http://localhost:9222"), "localhost:9222");
    }

    #[test]
    fn timestamps_are_utc_with_milliseconds() {
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(951_782_400_123)),
            "2000-02-29T00:00:00.123Z"
        );
    }

    #[test]
    fn a_log_writes_commands_answers_events_and_cancellations() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = lines.clone();
        let options = ProtocolLogOptions {
            sink: Some(Arc::new(move |line: &str| {
                seen.lock().unwrap().push(line.to_owned())
            })),
            ..Default::default()
        };
        let log = ProtocolLog::open(&options, "cdp-pipe", "pid 7").unwrap();
        log.command(1, None, "Browser.getVersion", &json!({}));
        log.response(1, Some(&json!({ "product": "Chrome/158" })), None);
        log.command(
            2,
            Some("S1"),
            "Page.navigate",
            &json!({ "url": "https://a.test" }),
        );
        log.response(2, None, Some((-32000, "Cannot navigate")));
        log.event(
            Some("S1"),
            "Page.loadEventFired",
            Some(&json!({ "timestamp": 1.5 })),
        );
        log.command(
            3,
            Some("S1"),
            "Runtime.evaluate",
            &json!({ "expression": "1" }),
        );
        log.malformed(b"not json\n");
        log.closed(Some("browser exited"));
        log.closed(None);

        let lines = lines.lock().unwrap().clone();
        let entries: Vec<String> = lines
            .iter()
            .map(|l| l.splitn(3, ' ').nth(2).unwrap().to_owned())
            .collect();
        assert!(
            lines[0].split(' ').nth(1).unwrap().starts_with('c'),
            "{}",
            lines[0]
        );
        assert_eq!(entries[0], "open cdp-pipe pid 7");
        assert_eq!(entries[1], "-> #1 - Browser.getVersion {}");
        assert!(
            entries[2].starts_with("<- #1 - Browser.getVersion "),
            "{}",
            entries[2]
        );
        assert!(
            entries[2].ends_with(r#"ms {"product":"Chrome/158"}"#),
            "{}",
            entries[2]
        );
        assert_eq!(
            entries[3],
            r#"-> #2 S1 Page.navigate {"url":"https://a.test"}"#
        );
        assert!(
            entries[4].ends_with(r#"ms error -32000 "Cannot navigate""#),
            "{}",
            entries[4]
        );
        assert_eq!(
            entries[5],
            r#"<- event S1 Page.loadEventFired {"timestamp":1.5}"#
        );
        assert_eq!(entries[7], r"<- malformed not json\n");
        assert!(
            entries[8].starts_with("xx #3 S1 Runtime.evaluate cancelled after "),
            "{}",
            entries[8]
        );
        assert_eq!(entries[9], "closed (browser exited)");
        assert_eq!(entries.len(), 10, "closing twice logs once: {entries:?}");
    }

    #[test]
    fn several_connections_share_one_file_with_one_header() {
        let path =
            std::env::temp_dir().join(format!("agito-log-{}/shared.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let options = ProtocolLogOptions::to(&path);
        let a = ProtocolLog::open(&options, "cdp-pipe", "pid 1").unwrap();
        let b = ProtocolLog::open(&options, "cdp-ws", "127.0.0.1:9222").unwrap();
        a.command(1, None, "Target.getTargets", &json!({}));
        b.command(1, None, "Browser.getVersion", &json!({}));
        a.closed(None);
        b.closed(None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("protocol log, process").count(), 1, "{text}");
        assert!(text.contains(&format!("{} -> #1 - Target.getTargets", a.tag)));
        assert!(text.contains(&format!("{} -> #1 - Browser.getVersion", b.tag)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "readable by its owner only");
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
