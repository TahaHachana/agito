//! What the session keeps for each tab: its latest snapshot, its console and network logs,
//! the dialog it is waiting on, and the frame tools are scoped to. All of it is attached when
//! the page opens, so the logs hold everything the page did, not only what happened while it
//! was the active tab.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agito::{Dialog, Frame, Page, Request, Response};
use tokio::sync::oneshot;

use crate::session::PageSnapshot;

/// How many entries each log keeps; older ones drop off the front.
const LOG_CAPACITY: usize = 250;
/// The longest request body a log entry keeps.
const POST_DATA_LIMIT: usize = 2000;
/// How long reading a response body may take.
const BODY_TIMEOUT: Duration = Duration::from_secs(5);

/// What becomes of a dialog a page raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogPolicy {
    /// Leave it open for `handle_dialog`. The default.
    #[default]
    Ask,
    /// Accept it at once.
    Accept,
    /// Dismiss it at once.
    Dismiss,
}

impl std::str::FromStr for DialogPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" => Ok(DialogPolicy::Ask),
            "accept" => Ok(DialogPolicy::Accept),
            "dismiss" => Ok(DialogPolicy::Dismiss),
            other => Err(format!(
                "unknown dialog policy '{other}': expected ask, accept or dismiss"
            )),
        }
    }
}

/// A bounded log whose entries carry sequence numbers, read from a cursor.
pub(crate) struct Log<T> {
    entries: VecDeque<(u64, T)>,
    next: u64,
}

/// What a read of a log returns: the entries from the cursor on, the cursor for the next read,
/// and how many asked-for entries had already dropped off the front.
pub(crate) struct Slice<T> {
    pub entries: Vec<(u64, T)>,
    pub next: u64,
    pub dropped: u64,
}

impl<T: Clone> Log<T> {
    fn push(&mut self, entry: T) {
        if self.entries.len() >= LOG_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((self.next, entry));
        self.next += 1;
    }

    pub fn next(&self) -> u64 {
        self.next
    }

    pub fn read(&self, since: Option<u64>) -> Slice<T> {
        let from = since.filter(|s| *s > 1).unwrap_or(1);
        let oldest = self.entries.front().map_or(self.next, |(seq, _)| *seq);
        Slice {
            entries: self
                .entries
                .iter()
                .filter(|(seq, _)| *seq >= from)
                .cloned()
                .collect(),
            next: self.next,
            dropped: oldest.saturating_sub(from),
        }
    }

    pub fn find(&self, sequence: u64) -> Option<T> {
        self.entries
            .iter()
            .find(|(seq, _)| *seq == sequence)
            .map(|(_, e)| e.clone())
    }
}

impl<T> Default for Log<T> {
    fn default() -> Self {
        Log {
            entries: VecDeque::new(),
            next: 1,
        }
    }
}

/// A console message or an uncaught error.
#[derive(Clone)]
pub(crate) struct ConsoleEntry {
    /// The console method (`log`, `error`, ...), or `pageerror` for an uncaught error.
    pub kind: String,
    pub text: String,
}

/// The type of an uncaught error's entry.
pub(crate) const PAGE_ERROR: &str = "pageerror";

impl std::fmt::Display for ConsoleEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.kind, self.text)
    }
}

/// A finished or failed request.
#[derive(Clone)]
pub(crate) struct NetworkEntry {
    pub method: String,
    /// `None` when the request failed.
    pub status: Option<u16>,
    pub url: String,
    pub resource_type: String,
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    pub post_data: Option<String>,
    /// For reading the body later, while the browser still has it.
    response: Option<Response>,
}

impl NetworkEntry {
    fn new(request: &Request, response: Option<&Response>) -> Self {
        let headers = |h: &std::collections::HashMap<String, String>| {
            let mut pairs: Vec<(String, String)> =
                h.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            pairs.sort();
            pairs
        };
        let post_data = request.post_data().filter(|d| !d.is_empty()).map(|d| {
            if d.len() <= POST_DATA_LIMIT {
                d.to_owned()
            } else {
                let mut end = POST_DATA_LIMIT;
                while !d.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &d[..end])
            }
        });
        NetworkEntry {
            method: request.method().to_owned(),
            status: response.map(Response::status),
            url: response.map_or(request.url(), Response::url).to_owned(),
            resource_type: request.resource_type().to_owned(),
            request_headers: headers(request.headers()),
            response_headers: response.map(|r| headers(r.headers())).unwrap_or_default(),
            post_data,
            response: response.cloned(),
        }
    }

    pub fn response_header(&self, name: &str) -> Option<&str> {
        self.response_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The response body as text, or `None` when the browser no longer has it.
    pub async fn body(&self) -> Option<String> {
        let response = self.response.as_ref()?;
        let bytes = tokio::time::timeout(BODY_TIMEOUT, response.body())
            .await
            .ok()?
            .ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl std::fmt::Display for NetworkEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self
            .status
            .map_or_else(|| "FAILED".to_owned(), |s| s.to_string());
        write!(f, "{} {status} {}", self.method, self.url)?;
        if !self.resource_type.is_empty() {
            write!(f, " ({})", self.resource_type)?;
        }
        Ok(())
    }
}

/// The dialog a page is waiting on, and a waiter to tell when one opens.
#[derive(Default)]
struct Dialogs {
    pending: Option<Dialog>,
    armed: Option<oneshot::Sender<Dialog>>,
}

/// One tab and everything the session keeps for it.
#[derive(Clone)]
pub(crate) struct Tab {
    pub page: Page,
    pub state: Arc<TabState>,
}

/// The per-tab state; see the module docs.
#[derive(Default)]
pub(crate) struct TabState {
    pub snapshot: tokio::sync::Mutex<PageSnapshot>,
    pub console: Mutex<Log<ConsoleEntry>>,
    pub network: Mutex<Log<NetworkEntry>>,
    dialogs: Mutex<Dialogs>,
    /// The frame `frame_select` scoped tools to, or `None` for the page.
    frame: Mutex<Option<Frame>>,
}

impl Tab {
    /// Starts keeping state for `page`, answering its dialogs by `policy`.
    pub fn attach(page: &Page, policy: DialogPolicy) -> Tab {
        let state = Arc::new(TabState::default());
        let console = Arc::downgrade(&state);
        page.on_console(move |message| {
            if let Some(state) = console.upgrade() {
                state.console.lock().unwrap().push(ConsoleEntry {
                    kind: message.kind.clone(),
                    text: message.text.clone(),
                });
            }
        });
        let errors = Arc::downgrade(&state);
        page.on_page_error(move |text| {
            if let Some(state) = errors.upgrade() {
                state.console.lock().unwrap().push(ConsoleEntry {
                    kind: PAGE_ERROR.to_owned(),
                    text: text.to_owned(),
                });
            }
        });
        let responses = Arc::downgrade(&state);
        page.on_response(move |response| {
            if let (Some(state), Some(request)) = (responses.upgrade(), response.request()) {
                let entry = NetworkEntry::new(request, Some(response));
                state.network.lock().unwrap().push(entry);
            }
        });
        let failures = Arc::downgrade(&state);
        page.on_request_failed(move |request| {
            if let Some(state) = failures.upgrade() {
                let entry = NetworkEntry::new(request, None);
                state.network.lock().unwrap().push(entry);
            }
        });
        let dialogs = Arc::downgrade(&state);
        page.on_dialog(move |dialog| {
            let state = dialogs.upgrade();
            async move {
                match (policy, state) {
                    (DialogPolicy::Accept, _) => {
                        let _ = dialog.accept(None).await;
                    }
                    (DialogPolicy::Dismiss, _) | (DialogPolicy::Ask, None) => {
                        let _ = dialog.dismiss().await;
                    }
                    (DialogPolicy::Ask, Some(state)) => {
                        let mut dialogs = state.dialogs.lock().unwrap();
                        dialogs.pending = Some(dialog.clone());
                        if let Some(waiter) = dialogs.armed.take() {
                            let _ = waiter.send(dialog);
                        }
                    }
                }
            }
        });
        Tab {
            page: page.clone(),
            state,
        }
    }
}

impl TabState {
    /// The dialog the page is waiting on.
    pub fn pending_dialog(&self) -> Option<Dialog> {
        self.dialogs.lock().unwrap().pending.clone()
    }

    /// Takes the pending dialog, to answer it.
    pub fn take_dialog(&self) -> Option<Dialog> {
        self.dialogs.lock().unwrap().pending.take()
    }

    /// A receiver told when a dialog opens, or at once if one already has.
    pub fn arm(&self) -> oneshot::Receiver<Dialog> {
        let (tx, rx) = oneshot::channel();
        let mut dialogs = self.dialogs.lock().unwrap();
        match &dialogs.pending {
            Some(dialog) => {
                let _ = tx.send(dialog.clone());
            }
            None => dialogs.armed = Some(tx),
        }
        rx
    }

    pub fn disarm(&self) {
        self.dialogs.lock().unwrap().armed = None;
    }

    /// The selected frame, unless it has gone.
    pub fn frame(&self) -> Option<Frame> {
        let mut frame = self.frame.lock().unwrap();
        if frame.as_ref().is_some_and(Frame::is_detached) {
            *frame = None;
        }
        frame.clone()
    }

    pub fn select_frame(&self, frame: Option<Frame>) {
        *self.frame.lock().unwrap() = frame;
    }
}

/// How a dialog's kind reads in a sentence.
fn dialog_name(dialog: &Dialog) -> &str {
    match dialog.kind.as_str() {
        "alert" | "confirm" | "prompt" | "beforeunload" => &dialog.kind,
        _ => "dialog",
    }
}

/// What an action that opened a dialog reports.
pub(crate) fn dialog_interrupted(dialog: &Dialog) -> String {
    let name = dialog_name(dialog);
    let article = if name.starts_with('a') { "an" } else { "a" };
    format!(
        "The action opened {article} {name} dialog: \"{}\". Call handle_dialog to accept or \
         dismiss it. The page finishes the action once the dialog is answered, so a dialog \
         opened from a mousedown handler means the click lands after this call returned.",
        dialog.message
    )
}

/// What a call reports while a dialog is open.
pub(crate) fn dialog_pending(dialog: &Dialog) -> String {
    format!(
        "A \"{}\" dialog is open: \"{}\". Handle it with handle_dialog before other actions.",
        dialog_name(dialog),
        dialog.message
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_read_from_a_cursor_and_count_what_dropped() {
        let mut log = Log::default();
        for i in 0..(LOG_CAPACITY + 5) {
            log.push(i);
        }
        let all = log.read(None);
        assert_eq!(all.entries.len(), LOG_CAPACITY);
        assert_eq!(all.dropped, 5);
        assert_eq!(all.next, LOG_CAPACITY as u64 + 6);
        let tail = log.read(Some(all.next - 2));
        assert_eq!(tail.entries.len(), 2);
        assert_eq!(tail.dropped, 0);
        assert!(log.read(Some(all.next)).entries.is_empty());
        assert_eq!(log.find(7), Some(6));
        assert_eq!(log.find(1), None);
    }

    #[test]
    fn policies_parse() {
        assert_eq!("Accept".parse(), Ok(DialogPolicy::Accept));
        assert!("maybe".parse::<DialogPolicy>().is_err());
    }
}
