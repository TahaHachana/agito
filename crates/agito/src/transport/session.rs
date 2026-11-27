//! Sessions: a protocol channel scoped to one target (the browser, a page, a worker).
//!
//! The engine speaks CDP method names through [`Session`]. A CDP session forwards them as-is;
//! the BiDi session translates them, so the rest of agito does not care which browser it drives.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agito_protocol::{Command, Event};
use async_trait::async_trait;
use serde_json::Value;

use super::capabilities::Capabilities;
use super::cdp::{CdpConnection, TransportError};
use super::events::{EventStream, TypedEventStream, event_key};
use crate::error::{Error, ErrorKind, Result};

/// A protocol session scoped to one target.
#[async_trait]
pub trait Session: Send + Sync + 'static {
    /// The session id, or `None` for the browser-level session.
    fn session_id(&self) -> Option<&str>;

    /// What the transport behind this session can do.
    fn capabilities(&self) -> Capabilities;

    /// A human description of the transport, such as `Chrome/CDP`, for error messages.
    fn transport_description(&self) -> &'static str;

    /// Sends a CDP command by name and returns the raw answer.
    async fn send_raw(&self, method: &str, params: Value) -> Result<Value>;

    /// Subscribes to the named events on this session, in wire order on one stream.
    fn subscribe(&self, methods: &[&str]) -> EventStream;

    /// Ends this session's event subscriptions, when its target has gone.
    fn cleanup_subscriptions(&self);
}

impl dyn Session {
    /// Sends a typed command and decodes the typed answer.
    pub async fn send<C: Command + Sync>(&self, command: &C) -> Result<C::Response> {
        let params = serde_json::to_value(command)
            .map_err(|e| Error::invalid_argument(format!("could not encode {}: {e}", C::METHOD)))?;
        let result = self.send_raw(C::METHOD, params).await?;
        serde_json::from_value(result).map_err(|e| {
            ErrorKind::Protocol {
                code: None,
                method: Some(C::METHOD.to_owned()),
                message: format!("could not read the answer to {}: {e}", C::METHOD),
            }
            .into()
        })
    }

    /// Subscribes to one kind of typed event on this session.
    pub fn events<E: Event>(&self) -> TypedEventStream<E> {
        self.subscribe(&[E::NAME]).typed()
    }
}

/// A CDP session: forwards commands over the shared connection with its session id.
pub struct CdpSession {
    connection: CdpConnection,
    session_id: Option<String>,
}

impl CdpSession {
    pub fn new(connection: CdpConnection, session_id: Option<String>) -> Self {
        Self {
            connection,
            session_id,
        }
    }

    pub fn connection(&self) -> &CdpConnection {
        &self.connection
    }
}

/// The description used in error messages for a CDP transport.
pub const CDP_DESCRIPTION: &str = "Chrome/CDP";

#[async_trait]
impl Session for CdpSession {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALL_CDP
    }

    fn transport_description(&self) -> &'static str {
        CDP_DESCRIPTION
    }

    async fn send_raw(&self, method: &str, params: Value) -> Result<Value> {
        self.connection
            .send_raw(method, &params, self.session_id.as_deref())
            .await
            .map_err(|e| map_transport_error(e, method, self.session_id.as_deref()))
    }

    fn subscribe(&self, methods: &[&str]) -> EventStream {
        let keys: Vec<String> = methods
            .iter()
            .map(|m| event_key(m, self.session_id.as_deref()))
            .collect();
        self.connection.subscribe(&keys)
    }

    fn cleanup_subscriptions(&self) {
        if let Some(id) = &self.session_id {
            self.connection.remove_session_subscriptions(id);
        }
    }
}

/// Maps a transport failure onto the public error type, as `CdpSession` does in Motus.
pub(crate) fn map_transport_error(
    error: TransportError,
    method: &str,
    session_id: Option<&str>,
) -> Error {
    match error {
        // Chrome's ways of saying the target went away under the command.
        TransportError::Protocol { message, .. } if is_target_gone(&message) => {
            Error::target_closed("session", session_id, message)
        }
        TransportError::Protocol { code, message } => ErrorKind::Protocol {
            code: Some(code),
            method: Some(method.to_owned()),
            message,
        }
        .into(),
        TransportError::Disconnected(_) | TransportError::SessionClosed => {
            Error::target_closed("session", session_id, error.to_string())
        }
        TransportError::Timeout(timeout) => ErrorKind::CommandTimeout {
            method: method.to_owned(),
            timeout,
        }
        .into(),
    }
}

fn is_target_gone(message: &str) -> bool {
    const GONE: &[&str] = &[
        "Session with given id not found",
        "Target closed",
        "No target with given id",
        "Inspected target navigated or closed",
    ];
    GONE.iter().any(|g| message.contains(g))
}

/// Keeps track of the sessions multiplexed over one connection.
pub trait SessionRegistry: Send + Sync + 'static {
    /// The browser-level session.
    fn browser_session(&self) -> Arc<dyn Session>;
    /// Creates and registers a session for a target.
    fn create_session(&self, session_id: &str) -> Arc<dyn Session>;
    fn get_session(&self, session_id: &str) -> Option<Arc<dyn Session>>;
    fn remove_session(&self, session_id: &str) -> bool;
}

/// The CDP session registry: one [`CdpSession`] per attached target.
pub struct CdpSessionRegistry {
    connection: CdpConnection,
    browser: Arc<dyn Session>,
    sessions: Mutex<HashMap<String, Arc<dyn Session>>>,
}

impl CdpSessionRegistry {
    pub fn new(connection: CdpConnection) -> Self {
        let browser: Arc<dyn Session> = Arc::new(CdpSession::new(connection.clone(), None));
        Self {
            connection,
            browser,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn connection(&self) -> &CdpConnection {
        &self.connection
    }

    pub fn active_sessions(&self) -> Vec<Arc<dyn Session>> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }
}

impl SessionRegistry for CdpSessionRegistry {
    fn browser_session(&self) -> Arc<dyn Session> {
        self.browser.clone()
    }

    fn create_session(&self, session_id: &str) -> Arc<dyn Session> {
        let session: Arc<dyn Session> = Arc::new(CdpSession::new(
            self.connection.clone(),
            Some(session_id.to_owned()),
        ));
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id.to_owned(), session.clone());
        session
    }

    fn get_session(&self, session_id: &str) -> Option<Arc<dyn Session>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    fn remove_session(&self, session_id: &str) -> bool {
        self.sessions.lock().unwrap().remove(session_id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeSocket;
    use crate::transport::cdp::ConnectionOptions;
    use agito_protocol::cdp::{page, runtime};
    use serde_json::json;

    fn registry() -> (FakeSocket, CdpSessionRegistry) {
        let (fake, pair) = FakeSocket::new();
        (
            fake,
            CdpSessionRegistry::new(CdpConnection::start(pair, ConnectionOptions::default())),
        )
    }

    #[tokio::test]
    async fn typed_send_round_trips() {
        let (fake, reg) = registry();
        let session = reg.create_session("S1");
        fake.respond_to(
            "Page.navigate",
            json!({ "frameId": "F1", "loaderId": "L1" }),
        );
        let result = session
            .send(&page::NavigateParams {
                url: "https://example.com".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.frame_id, "F1");
        let sent = fake.sent_command("Page.navigate", 0);
        assert_eq!(sent["sessionId"], "S1");
        assert_eq!(sent["params"], json!({ "url": "https://example.com" }));
    }

    #[tokio::test]
    async fn protocol_error_maps_to_protocol_variant() {
        let (fake, reg) = registry();
        fake.respond_error("Page.navigate", -32000, "Cannot navigate");
        let err = reg
            .browser_session()
            .send(&page::NavigateParams {
                url: "x".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        match err.kind() {
            ErrorKind::Protocol {
                code,
                method,
                message,
            } => {
                assert_eq!(*code, Some(-32000));
                assert_eq!(method.as_deref(), Some("Page.navigate"));
                assert_eq!(message, "Cannot navigate");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_maps_to_browser_lost() {
        let (fake, reg) = registry();
        let session = reg.create_session("S1");
        fake.disconnect();
        reg.connection().closed().await;
        let err = session
            .send(&page::EnableParams::default())
            .await
            .unwrap_err();
        assert!(err.is_browser_lost(), "{err:?}");
    }

    #[tokio::test]
    async fn missing_session_maps_to_browser_lost() {
        let (fake, reg) = registry();
        fake.respond_error("Page.enable", -32001, "Session with given id not found.");
        let err = reg
            .create_session("S9")
            .send(&page::EnableParams::default())
            .await
            .unwrap_err();
        assert!(err.is_browser_lost(), "{err:?}");
    }

    #[tokio::test]
    async fn malformed_answer_is_a_protocol_error() {
        let (fake, reg) = registry();
        fake.respond_to("Page.navigate", json!({ "frameId": 7 }));
        let err = reg
            .browser_session()
            .send(&page::NavigateParams {
                url: "x".into(),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::Protocol { code: None, .. }));
    }

    #[tokio::test]
    async fn typed_events_are_session_scoped() {
        let (fake, reg) = registry();
        let session = reg.create_session("S1");
        let mut events = session.events::<runtime::ExecutionContextsClearedEvent>();
        fake.enqueue(json!({ "method": "Runtime.executionContextsCleared", "sessionId": "S2", "params": {} }));
        fake.enqueue(json!({ "method": "Runtime.executionContextsCleared", "sessionId": "S1", "params": {} }));
        assert!(events.next().await.is_some());
        session.cleanup_subscriptions();
        assert!(events.next().await.is_none());
    }

    #[tokio::test]
    async fn registry_tracks_sessions() {
        let (_fake, reg) = registry();
        assert!(reg.browser_session().session_id().is_none());
        reg.create_session("A");
        assert!(reg.get_session("A").is_some());
        assert!(reg.remove_session("A"));
        assert!(!reg.remove_session("A"));
        assert!(reg.get_session("A").is_none());
    }
}
