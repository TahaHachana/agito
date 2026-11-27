//! The CDP connection: one socket, one receive loop, request/response correlation by id, and
//! event fan-out keyed by `"Domain.event|sessionId"`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::events::{EventStream, RawEvent, event_key};
use super::socket::{SocketPair, SocketReceiver, SocketSender};

/// How long a command may go unanswered before the browser is treated as unresponsive.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a command failed at the transport level.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TransportError {
    /// The connection is gone, with the reason if one is known.
    #[error("CDP connection closed{}", .0.as_deref().map(|r| format!(" ({r})")).unwrap_or_default())]
    Disconnected(Option<String>),
    /// The browser answered with an error.
    #[error("{message}")]
    Protocol {
        /// The protocol error code.
        code: i64,
        /// The browser's message.
        message: String,
    },
    /// No answer arrived in time.
    #[error("no answer within {0:?}")]
    Timeout(Duration),
    /// The session the command was sent on detached before the browser answered.
    #[error("the session closed before the browser answered")]
    SessionClosed,
}

/// Whether the connection is still usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Commands can be sent.
    Open,
    /// Closed, with the error that closed it, if it was not a clean close.
    Closed {
        /// The error that closed it, if it was not a clean close.
        error: Option<String>,
    },
}

/// Options for a CDP connection.
#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    /// Delay before every command, to slow automation down for a watcher.
    pub slow_mo: Duration,
    /// How long a command may wait for its answer.
    pub command_timeout: Duration,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            slow_mo: Duration::ZERO,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

/// A command waiting for its answer, with the session it was sent on.
struct Pending {
    tx: oneshot::Sender<Result<Value, TransportError>>,
    session_id: Option<String>,
}

/// A CDP connection. Cheap to clone; every clone talks over the same socket.
#[derive(Clone)]
pub struct CdpConnection {
    inner: Arc<Inner>,
}

struct Inner {
    sender: tokio::sync::Mutex<Box<dyn SocketSender>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    subscribers: Mutex<HashMap<String, Vec<mpsc::UnboundedSender<RawEvent>>>>,
    state: watch::Sender<ConnectionState>,
    options: ConnectionOptions,
    shutdown: CancellationToken,
    receive_task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Serialize)]
struct OutboundEnvelope<'a> {
    id: u64,
    method: &'a str,
    params: &'a Value,
    #[serde(rename = "sessionId", skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

#[derive(Deserialize)]
struct InboundEnvelope {
    id: Option<u64>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<ErrorPayload>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct ErrorPayload {
    code: i64,
    message: String,
}

impl CdpConnection {
    /// Takes over an open socket and starts the receive loop. Must be called inside a tokio
    /// runtime, which then owns the loop.
    pub fn start(socket: SocketPair, options: ConnectionOptions) -> Self {
        let (state, _) = watch::channel(ConnectionState::Open);
        let inner = Arc::new(Inner {
            sender: tokio::sync::Mutex::new(socket.sender),
            next_id: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            subscribers: Mutex::new(HashMap::new()),
            state,
            options,
            shutdown: CancellationToken::new(),
            receive_task: Mutex::new(None),
        });
        let task = tokio::spawn(receive_loop(
            Arc::downgrade(&inner),
            socket.receiver,
            inner.shutdown.clone(),
        ));
        *inner.receive_task.lock().unwrap() = Some(task);
        Self { inner }
    }

    /// Whether the connection has been lost or closed.
    pub fn is_closed(&self) -> bool {
        !matches!(*self.inner.state.borrow(), ConnectionState::Open)
    }

    /// Watches the connection state; resolves `changed()` when it closes.
    pub fn state(&self) -> watch::Receiver<ConnectionState> {
        self.inner.state.subscribe()
    }

    /// Resolves once the connection is closed, with the error that closed it, if any.
    pub async fn closed(&self) -> Option<String> {
        let mut rx = self.state();
        loop {
            if let ConnectionState::Closed { error } = &*rx.borrow_and_update() {
                return error.clone();
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    /// Sends a command and waits for the browser's answer.
    pub async fn send_raw(
        &self,
        method: &str,
        params: &Value,
        session_id: Option<&str>,
    ) -> Result<Value, TransportError> {
        if let ConnectionState::Closed { error } = &*self.inner.state.borrow() {
            return Err(TransportError::Disconnected(error.clone()));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(
            id,
            Pending {
                tx,
                session_id: session_id.map(str::to_owned),
            },
        );
        let _guard = PendingGuard {
            inner: &self.inner,
            id,
        };
        // The connection may have closed between the check above and registering; faulting
        // happened before we were in the map, so check again rather than wait out the timeout.
        if let ConnectionState::Closed { error } = &*self.inner.state.borrow() {
            return Err(TransportError::Disconnected(error.clone()));
        }

        let timeout = self.inner.options.command_timeout;
        let exchange = async {
            if !self.inner.options.slow_mo.is_zero() {
                tokio::time::sleep(self.inner.options.slow_mo).await;
            }
            let envelope = OutboundEnvelope {
                id,
                method,
                params,
                session_id,
            };
            let text = serde_json::to_string(&envelope).expect("JSON values always serialize");
            tracing::trace!(target: "agito::protocol", id, method, session_id, "send");
            {
                let mut sender = self.inner.sender.lock().await;
                sender
                    .send(text)
                    .await
                    .map_err(|e| TransportError::Disconnected(Some(e.to_string())))?;
            }
            rx.await.unwrap_or(Err(TransportError::Disconnected(None)))
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::Timeout(timeout)),
        }
    }

    /// Subscribes to every event whose key is in `keys`. One stream carries them all, in the
    /// order the browser sent them, which matters for events that describe one structure
    /// between them (a frame attaching, then its parent navigating).
    ///
    /// Each key is `"Domain.event|sessionId"`, with an empty session id for browser-level
    /// events; see [`event_key`].
    pub fn subscribe(&self, keys: &[String]) -> EventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        if self.is_closed() {
            return EventStream::new(rx);
        }
        let mut subscribers = self.inner.subscribers.lock().unwrap();
        for key in keys {
            subscribers.entry(key.clone()).or_default().push(tx.clone());
        }
        EventStream::new(rx)
    }

    /// Ends every subscription scoped to `session_id`, for a page or worker that has gone.
    pub fn remove_session_subscriptions(&self, session_id: &str) {
        let suffix = format!("|{session_id}");
        self.inner
            .subscribers
            .lock()
            .unwrap()
            .retain(|key, _| !key.ends_with(&suffix));
    }

    /// Closes the connection: stops the receive loop, fails anything still waiting and closes
    /// the socket (for a pipe, that tells the browser to exit).
    pub async fn close(&self) {
        self.inner.shutdown.cancel();
        let task = self.inner.receive_task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        mark_closed(&self.inner, None);
        self.inner.sender.lock().await.close().await;
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.inner.pending.lock().unwrap().len()
    }
}

/// Takes a command out of the pending map however its send ends, including cancellation.
struct PendingGuard<'a> {
    inner: &'a Inner,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.inner.pending.lock().unwrap().remove(&self.id);
    }
}

async fn receive_loop(
    inner: std::sync::Weak<Inner>,
    mut receiver: Box<dyn SocketReceiver>,
    shutdown: CancellationToken,
) {
    loop {
        let message = tokio::select! {
            _ = shutdown.cancelled() => return,
            message = receiver.recv() => message,
        };
        let Some(inner) = inner.upgrade() else { return };
        match message {
            Ok(Some(bytes)) => match serde_json::from_slice::<InboundEnvelope>(&bytes) {
                Ok(envelope) => dispatch(&inner, envelope),
                Err(e) => {
                    tracing::debug!(target: "agito::protocol", error = %e, "skipping malformed message")
                }
            },
            Ok(None) => {
                tracing::debug!(target: "agito::protocol", "connection closed by the browser");
                mark_closed(&inner, None);
                return;
            }
            Err(e) => {
                tracing::debug!(target: "agito::protocol", error = %e, "connection lost");
                mark_closed(&inner, Some(e.to_string()));
                return;
            }
        }
    }
}

fn dispatch(inner: &Inner, envelope: InboundEnvelope) {
    if let Some(id) = envelope.id {
        let Some(Pending { tx, .. }) = inner.pending.lock().unwrap().remove(&id) else {
            return;
        };
        let result = match envelope.error {
            Some(error) => Err(TransportError::Protocol {
                code: error.code,
                message: error.message,
            }),
            None => Ok(envelope
                .result
                .unwrap_or_else(|| Value::Object(Default::default()))),
        };
        let _ = tx.send(result);
    } else if let Some(method) = envelope.method {
        if method == "Target.detachedFromTarget"
            && let Some(gone) = envelope
                .params
                .as_ref()
                .and_then(|p| p.get("sessionId"))
                .and_then(Value::as_str)
        {
            fail_session_commands(inner, gone);
        }
        let key = event_key(&method, envelope.session_id.as_deref());
        let mut subscribers = inner.subscribers.lock().unwrap();
        if let Some(list) = subscribers.get_mut(&key) {
            let event = RawEvent {
                method,
                params: envelope.params.unwrap_or(Value::Null),
                session_id: envelope.session_id,
            };
            list.retain(|tx| tx.send(event.clone()).is_ok());
            if list.is_empty() {
                subscribers.remove(&key);
            }
        }
    }
}

/// Fails the commands still waiting on a session that detached. Chrome does not always answer
/// commands on a target it is destroying: a click whose mouse release runs `window.close()`
/// would otherwise wait out the whole command timeout.
fn fail_session_commands(inner: &Inner, session_id: &str) {
    let gone: Vec<Pending> = {
        let mut pending = inner.pending.lock().unwrap();
        let ids: Vec<u64> = pending
            .iter()
            .filter(|(_, p)| p.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| *id)
            .collect();
        ids.iter().filter_map(|id| pending.remove(id)).collect()
    };
    for p in gone {
        let _ = p.tx.send(Err(TransportError::SessionClosed));
    }
}

/// The single entry point for "the connection is gone": flips the state so later sends fail
/// fast, fails pending commands, and ends every event stream.
fn mark_closed(inner: &Inner, error: Option<String>) {
    let changed = inner.state.send_if_modified(|state| {
        if matches!(state, ConnectionState::Open) {
            *state = ConnectionState::Closed {
                error: error.clone(),
            };
            true
        } else {
            false
        }
    });
    let reason = match &*inner.state.borrow() {
        ConnectionState::Closed { error } => error.clone(),
        ConnectionState::Open => None,
    };
    let pending: Vec<Pending> = inner
        .pending
        .lock()
        .unwrap()
        .drain()
        .map(|(_, p)| p)
        .collect();
    for Pending { tx, .. } in pending {
        let _ = tx.send(Err(TransportError::Disconnected(reason.clone())));
    }
    inner.subscribers.lock().unwrap().clear();
    if changed {
        tracing::debug!(target: "agito::protocol", ?reason, "connection marked closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSocket;
    use serde_json::json;
    use std::time::Instant;

    fn connect() -> (FakeSocket, CdpConnection) {
        let (fake, pair) = FakeSocket::new();
        (
            fake,
            CdpConnection::start(pair, ConnectionOptions::default()),
        )
    }

    #[tokio::test]
    async fn correlates_response_by_id() {
        let (fake, conn) = connect();
        fake.queue_response(json!({ "result": { "frameId": "ABC" } }));
        let result = conn
            .send_raw("Page.navigate", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(result["frameId"], "ABC");
    }

    #[tokio::test]
    async fn protocol_error_is_returned() {
        let (fake, conn) = connect();
        fake.queue_response(json!({ "error": { "code": -32000, "message": "Page not found" } }));
        let err = conn
            .send_raw("Page.navigate", &json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            TransportError::Protocol {
                code: -32000,
                message: "Page not found".into()
            }
        );
    }

    #[tokio::test]
    async fn envelope_carries_session_id_only_when_set() {
        let (fake, conn) = connect();
        fake.queue_response(json!({ "result": {} }));
        fake.queue_response(json!({ "result": {} }));
        conn.send_raw("DOM.getDocument", &json!({}), Some("session-42"))
            .await
            .unwrap();
        conn.send_raw("Target.getTargets", &json!({}), None)
            .await
            .unwrap();
        let sent = fake.sent();
        assert_eq!(sent[0]["sessionId"], "session-42");
        assert_eq!(sent[0]["method"], "DOM.getDocument");
        assert!(sent[1].get("sessionId").is_none());
    }

    #[tokio::test]
    async fn ids_increase_monotonically() {
        let (fake, conn) = connect();
        fake.queue_response(json!({ "result": {} }));
        fake.queue_response(json!({ "result": {} }));
        conn.send_raw("a", &json!({}), None).await.unwrap();
        conn.send_raw("b", &json!({}), None).await.unwrap();
        let sent = fake.sent();
        assert_eq!(sent[0]["id"], 1);
        assert_eq!(sent[1]["id"], 2);
    }

    #[tokio::test]
    async fn missing_result_is_empty_object() {
        let (fake, conn) = connect();
        fake.queue_response(json!({}));
        let result = conn
            .send_raw("Page.enable", &json!({}), None)
            .await
            .unwrap();
        assert_eq!(result, json!({}));
    }

    #[tokio::test]
    async fn delivers_matching_events_only() {
        let (fake, conn) = connect();
        let mut events = conn.subscribe(&[event_key("Page.loadEventFired", None)]);
        fake.enqueue(
            json!({ "method": "Network.requestWillBeSent", "params": { "requestId": "1" } }),
        );
        fake.enqueue(json!({ "method": "Page.loadEventFired", "params": { "timestamp": 123.45 } }));
        let event = events.next().await.unwrap();
        assert_eq!(event.method, "Page.loadEventFired");
        assert_eq!(event.params["timestamp"], 123.45);
    }

    #[tokio::test]
    async fn events_are_scoped_by_session() {
        let (fake, conn) = connect();
        let mut events = conn.subscribe(&[event_key("Page.loadEventFired", Some("s1"))]);
        fake.enqueue(
            json!({ "method": "Page.loadEventFired", "sessionId": "s2", "params": { "n": 2 } }),
        );
        fake.enqueue(
            json!({ "method": "Page.loadEventFired", "sessionId": "s1", "params": { "n": 1 } }),
        );
        assert_eq!(events.next().await.unwrap().params["n"], 1);
    }

    #[tokio::test]
    async fn shared_subscription_keeps_wire_order() {
        let (fake, conn) = connect();
        let keys: Vec<String> = [
            "Page.frameNavigated",
            "Page.frameAttached",
            "Page.frameDetached",
        ]
        .iter()
        .map(|m| event_key(m, Some("s1")))
        .collect();
        let mut events = conn.subscribe(&keys);
        for (method, n) in [
            ("Page.frameNavigated", 1),
            ("Page.frameAttached", 2),
            ("Page.frameDetached", 3),
            ("Page.frameAttached", 4),
        ] {
            fake.enqueue(json!({ "method": method, "sessionId": "s1", "params": { "n": n } }));
        }
        let mut seen = Vec::new();
        for _ in 0..4 {
            let e = events.next().await.unwrap();
            seen.push(format!("{}:{}", e.method, e.params["n"]));
        }
        assert_eq!(
            seen,
            [
                "Page.frameNavigated:1",
                "Page.frameAttached:2",
                "Page.frameDetached:3",
                "Page.frameAttached:4"
            ]
        );
    }

    #[tokio::test]
    async fn every_subscriber_gets_every_event() {
        let (fake, conn) = connect();
        let key = event_key("Runtime.consoleAPICalled", None);
        let mut a = conn.subscribe(std::slice::from_ref(&key));
        let mut b = conn.subscribe(&[key]);
        fake.enqueue(json!({ "method": "Runtime.consoleAPICalled", "params": {} }));
        assert!(a.next().await.is_some());
        assert!(b.next().await.is_some());
    }

    #[tokio::test]
    async fn disconnect_fails_pending_sends() {
        let (fake, conn) = connect();
        let sender = conn.clone();
        let send =
            tokio::spawn(async move { sender.send_raw("Page.navigate", &json!({}), None).await });
        fake.wait_for_sent(1).await;
        fake.disconnect();
        assert!(matches!(
            send.await.unwrap(),
            Err(TransportError::Disconnected(_))
        ));
        assert_eq!(conn.pending_count(), 0);
    }

    #[tokio::test]
    async fn a_detached_session_fails_only_its_own_pending_sends() {
        let (fake, conn) = connect();
        let (a, b) = (conn.clone(), conn.clone());
        let on_gone = tokio::spawn(async move {
            a.send_raw("Input.dispatchMouseEvent", &json!({}), Some("gone"))
                .await
        });
        let on_other = tokio::spawn(async move {
            b.send_raw("Runtime.evaluate", &json!({}), Some("other"))
                .await
        });
        fake.wait_for_sent(2).await;

        fake.enqueue(json!({
            "method": "Target.detachedFromTarget",
            "params": { "sessionId": "gone", "targetId": "T" }
        }));
        assert_eq!(on_gone.await.unwrap(), Err(TransportError::SessionClosed));
        assert!(
            !on_other.is_finished(),
            "another session's command keeps waiting"
        );

        let id = fake.wait_for_command("Runtime.evaluate").await["id"].clone();
        fake.enqueue(json!({ "id": id, "result": { "ok": true } }));
        assert_eq!(on_other.await.unwrap(), Ok(json!({ "ok": true })));
        assert_eq!(conn.pending_count(), 0);
    }

    #[tokio::test]
    async fn sends_fail_fast_after_disconnect() {
        let (fake, conn) = connect();
        fake.disconnect();
        conn.closed().await;
        let started = Instant::now();
        let err = conn
            .send_raw("Page.navigate", &json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Disconnected(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn disconnect_ends_event_streams_and_reports_close() {
        let (fake, conn) = connect();
        let mut events = conn.subscribe(&[event_key("Page.loadEventFired", None)]);
        fake.disconnect();
        assert!(events.next().await.is_none());
        assert_eq!(conn.closed().await, None);
        assert!(conn.is_closed());
    }

    #[tokio::test]
    async fn removing_session_subscriptions_ends_their_streams() {
        let (_fake, conn) = connect();
        let mut scoped = conn.subscribe(&[event_key("Page.loadEventFired", Some("s1"))]);
        conn.remove_session_subscriptions("s1");
        assert!(scoped.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_command_times_out() {
        let (fake, pair) = FakeSocket::new();
        let conn = CdpConnection::start(
            pair,
            ConnectionOptions {
                command_timeout: Duration::from_secs(5),
                ..Default::default()
            },
        );
        let err = conn
            .send_raw("Page.navigate", &json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(err, TransportError::Timeout(Duration::from_secs(5)));
        assert_eq!(conn.pending_count(), 0);
        drop(fake);
    }

    #[tokio::test]
    async fn close_closes_socket_and_fails_later_sends() {
        let (fake, conn) = connect();
        conn.close().await;
        assert!(fake.is_closed_by_client());
        assert!(conn.send_raw("x", &json!({}), None).await.is_err());
    }
}
