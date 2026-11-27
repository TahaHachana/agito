//! Event streams handed out by a transport.

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use agito_protocol::Event;
use futures::Stream;
use serde_json::Value;
use tokio::sync::mpsc;

/// An event as it came off the wire, before typed deserialization.
#[derive(Debug, Clone, PartialEq)]
pub struct RawEvent {
    /// The event's name, such as `Page.loadEventFired`.
    pub method: String,
    /// The event's parameters.
    pub params: Value,
    /// The session it came from; `None` for the browser's.
    pub session_id: Option<String>,
}

/// The subscription key for an event: `"Domain.event|sessionId"`, with an empty session id for
/// browser-level events.
pub fn event_key(method: &str, session_id: Option<&str>) -> String {
    format!("{method}|{}", session_id.unwrap_or(""))
}

/// A stream of raw events. Ends when the subscription is removed or the connection closes.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<RawEvent>,
}

impl EventStream {
    pub(crate) fn new(rx: mpsc::UnboundedReceiver<RawEvent>) -> Self {
        Self { rx }
    }

    /// Waits for the next event, or `None` once the stream has ended.
    pub async fn next(&mut self) -> Option<RawEvent> {
        self.rx.recv().await
    }

    /// Turns this into a stream of typed events of one kind.
    pub fn typed<E: Event>(self) -> TypedEventStream<E> {
        TypedEventStream {
            inner: self,
            _marker: PhantomData,
        }
    }
}

impl Stream for EventStream {
    type Item = RawEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<RawEvent>> {
        self.rx.poll_recv(cx)
    }
}

/// A stream of typed events. Payloads that do not match the expected shape are skipped.
pub struct TypedEventStream<E> {
    inner: EventStream,
    _marker: PhantomData<fn() -> E>,
}

impl<E: Event> TypedEventStream<E> {
    /// Waits for the next event, or `None` once the stream has ended.
    pub async fn next(&mut self) -> Option<E> {
        loop {
            let raw = self.inner.next().await?;
            if let Some(event) = decode(raw) {
                return Some(event);
            }
        }
    }
}

impl<E: Event> Stream for TypedEventStream<E> {
    type Item = E;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<E>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(raw)) => {
                    if let Some(event) = decode(raw) {
                        return Poll::Ready(Some(event));
                    }
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn decode<E: Event>(raw: RawEvent) -> Option<E> {
    match serde_json::from_value(raw.params) {
        Ok(event) => Some(event),
        Err(e) => {
            tracing::warn!(target: "agito::protocol", event = E::NAME, error = %e, "skipping event with unexpected shape");
            None
        }
    }
}
