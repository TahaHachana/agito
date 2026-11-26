//! Test doubles for driving agito without a browser.
//!
//! [`FakeSocket`] stands in for the browser end of a connection: it records every command,
//! answers commands from queued or per-method responses, and lets a test push events or
//! simulate the browser going away.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::transport::socket::{SocketPair, SocketReceiver, SocketSender};

/// The browser end of a fake connection. Cheap to clone.
#[derive(Clone)]
pub struct FakeSocket {
    state: Arc<State>,
}

struct State {
    inbox: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    sent: Mutex<Vec<Value>>,
    sent_changed: Notify,
    queued: Mutex<VecDeque<Value>>,
    by_method: Mutex<HashMap<String, Value>>,
    closed_by_client: AtomicBool,
}

impl FakeSocket {
    /// A fake browser and the socket pair to hand to a transport.
    pub fn new() -> (Self, SocketPair) {
        let (tx, rx) = mpsc::unbounded_channel();
        let state = Arc::new(State {
            inbox: Mutex::new(Some(tx)),
            sent: Mutex::new(Vec::new()),
            sent_changed: Notify::new(),
            queued: Mutex::new(VecDeque::new()),
            by_method: Mutex::new(HashMap::new()),
            closed_by_client: AtomicBool::new(false),
        });
        let pair = SocketPair::new(
            FakeSender {
                state: state.clone(),
            },
            FakeReceiver { rx },
        );
        (Self { state }, pair)
    }

    /// Answers the next command that has no per-method answer with `envelope` (for example
    /// `{"result": {...}}` or `{"error": {...}}`); its `id` is filled in.
    pub fn queue_response(&self, envelope: Value) {
        self.state.queued.lock().unwrap().push_back(envelope);
    }

    /// Answers every `method` command with `result`.
    pub fn respond_to(&self, method: &str, result: Value) {
        self.state
            .by_method
            .lock()
            .unwrap()
            .insert(method.to_owned(), json!({ "result": result }));
    }

    /// Answers every `method` command with a protocol error.
    pub fn respond_error(&self, method: &str, code: i64, message: &str) {
        self.state.by_method.lock().unwrap().insert(
            method.to_owned(),
            json!({ "error": { "code": code, "message": message } }),
        );
    }

    /// Delivers a message (usually an event) from the browser.
    pub fn enqueue(&self, message: Value) {
        if let Some(tx) = &*self.state.inbox.lock().unwrap() {
            let _ = tx.send(message.to_string().into_bytes());
        }
    }

    /// Every command sent so far, as parsed JSON envelopes.
    pub fn sent(&self) -> Vec<Value> {
        self.state.sent.lock().unwrap().clone()
    }

    /// The `occurrence`-th command sent with `method`. Panics if there is none.
    pub fn sent_command(&self, method: &str, occurrence: usize) -> Value {
        let matches: Vec<Value> = self
            .sent()
            .into_iter()
            .filter(|m| m["method"] == method)
            .collect();
        matches.get(occurrence).cloned().unwrap_or_else(|| {
            panic!(
                "expected at least {} sent {method} command(s), found {}",
                occurrence + 1,
                matches.len()
            )
        })
    }

    /// The methods of every command sent so far, in order.
    pub fn sent_methods(&self) -> Vec<String> {
        self.sent()
            .iter()
            .filter_map(|m| m["method"].as_str().map(str::to_owned))
            .collect()
    }

    /// Waits until at least `count` commands have been sent.
    pub async fn wait_for_sent(&self, count: usize) {
        loop {
            let notified = self.state.sent_changed.notified();
            if self.state.sent.lock().unwrap().len() >= count {
                return;
            }
            notified.await;
        }
    }

    /// Waits until a command with `method` has been sent, and returns it.
    pub async fn wait_for_command(&self, method: &str) -> Value {
        loop {
            let notified = self.state.sent_changed.notified();
            if let Some(m) = self.sent().into_iter().find(|m| m["method"] == method) {
                return m;
            }
            notified.await;
        }
    }

    /// Simulates the browser closing the connection.
    pub fn disconnect(&self) {
        self.state.inbox.lock().unwrap().take();
    }

    /// Whether the client side closed the connection.
    pub fn is_closed_by_client(&self) -> bool {
        self.state.closed_by_client.load(Ordering::SeqCst)
    }
}

struct FakeSender {
    state: Arc<State>,
}

#[async_trait]
impl SocketSender for FakeSender {
    async fn send(&mut self, message: String) -> std::io::Result<()> {
        let command: Value = serde_json::from_str(&message).map_err(std::io::Error::other)?;
        let method = command["method"].as_str().unwrap_or_default().to_owned();
        let id = command["id"].clone();
        self.state.sent.lock().unwrap().push(command);
        self.state.sent_changed.notify_waiters();

        let answer = self
            .state
            .by_method
            .lock()
            .unwrap()
            .get(&method)
            .cloned()
            .or_else(|| self.state.queued.lock().unwrap().pop_front());
        if let Some(mut answer) = answer {
            if let Some(obj) = answer.as_object_mut() {
                obj.insert("id".into(), id);
            }
            if let Some(tx) = &*self.state.inbox.lock().unwrap() {
                let _ = tx.send(answer.to_string().into_bytes());
            }
        }
        Ok(())
    }

    async fn close(&mut self) {
        self.state.closed_by_client.store(true, Ordering::SeqCst);
        self.state.inbox.lock().unwrap().take();
    }
}

struct FakeReceiver {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[async_trait]
impl SocketReceiver for FakeReceiver {
    async fn recv(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.rx.recv().await)
    }
}
