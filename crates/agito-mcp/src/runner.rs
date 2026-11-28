//! Running an action against a page that may stop on a dialog, and reporting what it changed.
//!
//! A JavaScript dialog stops the page answering anything, the command that opened it included,
//! so an action is raced against a dialog opening: when one does, the call returns at once and
//! says so, and the action finishes in the background once the dialog is answered.
//!
//! A successful action then reports what it changed, in rows under its own line: where the
//! page went, tabs it opened, errors it logged, a dialog it left open, refs it retired. That
//! saves the snapshot, console read and tab listing it would otherwise take to find out.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::session::Session;
use crate::tab::{PAGE_ERROR, Tab, dialog_interrupted, dialog_pending};

/// How an action's own work ends: its line of text, or why it failed.
pub(crate) type Work = Result<String, String>;

/// How long to wait for a new tab's address after it appears.
const NEW_TAB_URL_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(25);
/// How long a result waits for a page to say what it is called.
const TITLE_TIMEOUT: Duration = Duration::from_secs(2);

/// How a guarded piece of work ended.
pub(crate) enum Guarded {
    /// It finished, with its own outcome.
    Done(Work),
    /// A dialog opened first; the notice to return. The work carries on in the background.
    Interrupted(String),
}

/// Runs `work` unless the tab is waiting on a dialog (`Err` with the notice), racing it
/// against one opening.
pub(crate) async fn guarded<F>(tab: &Tab, work: F) -> Result<Guarded, String>
where
    F: Future<Output = Work> + Send + 'static,
{
    if let Some(dialog) = tab.state.pending_dialog() {
        return Err(dialog_pending(&dialog));
    }
    let opened = tab.state.arm();
    let mut task = tokio::spawn(work);
    tokio::select! {
        joined = &mut task => {
            tab.state.disarm();
            Ok(Guarded::Done(
                joined.unwrap_or_else(|e| Err(format!("The action stopped: {e}"))),
            ))
        }
        Ok(dialog) = opened => Ok(Guarded::Interrupted(dialog_interrupted(&dialog))),
    }
}

/// The page's title, or `None` when it will not say in time, or cannot while a dialog is
/// open: a title is a nicety, not worth delaying the call that was only naming the page.
pub(crate) async fn title(tab: &Tab) -> Option<String> {
    if tab.state.pending_dialog().is_some() {
        return None;
    }
    match tokio::time::timeout(TITLE_TIMEOUT, tab.page.title()).await {
        Ok(Ok(title)) if !title.is_empty() => Some(title),
        _ => None,
    }
}

/// A page as `url | title`, or its URL alone.
pub(crate) fn describe_as(url: &str, title: Option<&str>) -> String {
    match title {
        Some(title) => format!("{url} | {title}"),
        None => url.to_owned(),
    }
}

/// A page as it is now.
pub(crate) async fn describe(tab: &Tab) -> String {
    describe_as(&tab.page.url(), title(tab).await.as_deref())
}

/// What the page looked like before an action, to report what the action changed.
pub(crate) struct Report {
    url: String,
    title: Option<String>,
    console_cursor: u64,
    had_snapshot: bool,
    frames_moved_before: Vec<usize>,
    scope_moved_before: bool,
    tabs_before: HashSet<String>,
    settle: Duration,
}

impl Report {
    pub async fn begin(session: &Arc<Mutex<Session>>, tab: &Tab, settle: Duration) -> Report {
        let tabs_before = session
            .lock()
            .await
            .tabs()
            .into_iter()
            .map(|(_, t)| t.page.target_id().to_owned())
            .collect();
        let snapshot = tab.state.snapshot.lock().await;
        Report {
            url: tab.page.url(),
            title: title(tab).await,
            console_cursor: tab.state.console.lock().unwrap().next(),
            had_snapshot: snapshot.taken(),
            frames_moved_before: snapshot.moved_frames(),
            scope_moved_before: snapshot.scope_moved(),
            tabs_before,
            settle,
        }
    }

    /// The rows to add under `first_line`, the action's own line.
    pub async fn rows(
        &self,
        first_line: &str,
        session: &Arc<Mutex<Session>>,
        tab: &Tab,
    ) -> Vec<String> {
        let mut rows = Vec::new();
        let pending = tab.state.pending_dialog();
        if let Some(dialog) = &pending {
            rows.push(dialog_pending(dialog));
        }
        let new_tabs = self.new_tabs(session, pending.is_some()).await;

        let url = tab.page.url();
        let title = match pending {
            Some(_) => self.title.clone(),
            None => title(tab).await,
        };
        let moved = url != self.url;
        let already_said =
            first_line.contains(&url) && title.as_deref().is_none_or(|t| first_line.contains(t));
        if (moved || title != self.title) && !already_said {
            rows.push(format!("Page: {}", describe_as(&url, title.as_deref())));
        }
        for (index, url) in new_tabs {
            rows.push(format!("New tab opened: [{index}] {url}"));
        }
        if let Some(console) = self.console_row(tab) {
            rows.push(console);
        }

        let mut snapshot = tab.state.snapshot.lock().await;
        if moved && self.had_snapshot {
            rows.push(
                "Refs from the last snapshot no longer address this page: it navigated. Take a \
                 new snapshot."
                    .into(),
            );
            snapshot.retire();
            tab.state.select_frame(None);
        } else {
            let frames: Vec<usize> = snapshot
                .moved_frames()
                .into_iter()
                .filter(|i| !self.frames_moved_before.contains(i))
                .collect();
            if !frames.is_empty() {
                let which = match frames.as_slice() {
                    [one] => format!("frame {one}"),
                    many => format!(
                        "frames {}",
                        many.iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                rows.push(format!(
                    "Refs inside {which} no longer address anything: the frame navigated. Take a \
                     new snapshot."
                ));
            } else if snapshot.scope_moved() && !self.scope_moved_before {
                rows.push(
                    "Refs from the last snapshot no longer address anything: the frame it \
                     described navigated. Take a new snapshot."
                        .into(),
                );
            }
        }
        rows
    }

    /// Tabs opened since the action began, by index: waited for up to the settle time, since
    /// a page opens a tab a moment after the click that asked for it, then given a moment to
    /// leave `about:blank` for the address they were opened at.
    async fn new_tabs(&self, session: &Arc<Mutex<Session>>, blocked: bool) -> Vec<(usize, String)> {
        let started = Instant::now();
        let opened = loop {
            let opened: Vec<(usize, Tab)> = session
                .lock()
                .await
                .tabs()
                .into_iter()
                .enumerate()
                .filter(|(_, (_, t))| !self.tabs_before.contains(t.page.target_id()))
                .map(|(i, (_, t))| (i, t))
                .collect();
            if !opened.is_empty() || blocked || started.elapsed() >= self.settle {
                break opened;
            }
            tokio::time::sleep(POLL).await;
        };
        let blank = |url: &str| url.is_empty() || url == "about:blank";
        let waited = Instant::now();
        while !blocked
            && opened.iter().any(|(_, t)| blank(&t.page.url()))
            && waited.elapsed() < NEW_TAB_URL_WAIT
        {
            tokio::time::sleep(POLL).await;
        }
        opened.into_iter().map(|(i, t)| (i, t.page.url())).collect()
    }

    fn console_row(&self, tab: &Tab) -> Option<String> {
        let slice = tab
            .state
            .console
            .lock()
            .unwrap()
            .read(Some(self.console_cursor));
        let page_errors = slice
            .entries
            .iter()
            .filter(|(_, e)| e.kind == PAGE_ERROR)
            .count();
        let errors = slice
            .entries
            .iter()
            .filter(|(_, e)| e.kind == "error")
            .count();
        if errors + page_errors == 0 {
            return None;
        }
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let mut counted = Vec::new();
        if errors > 0 {
            counted.push(plural(errors, "error"));
        }
        if page_errors > 0 {
            counted.push(plural(page_errors, "page error"));
        }
        Some(format!(
            "Console: {}. Read them with console_messages since={}.",
            counted.join(", "),
            self.console_cursor
        ))
    }
}
