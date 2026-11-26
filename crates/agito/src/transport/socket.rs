//! The byte-level connection to a browser, split into a sending and a receiving half so the
//! receive loop can own one while callers share the other.

use async_trait::async_trait;

/// The sending half of a connection. Calls are serialized by the transport.
#[async_trait]
pub trait SocketSender: Send + 'static {
    /// Sends one complete message.
    async fn send(&mut self, message: String) -> std::io::Result<()>;

    /// Closes the connection. For a pipe this is what tells the browser to exit.
    async fn close(&mut self);
}

/// The receiving half of a connection, owned by the transport's receive loop.
#[async_trait]
pub trait SocketReceiver: Send + 'static {
    /// Receives one complete message, or `None` once the peer has closed the connection.
    async fn recv(&mut self) -> std::io::Result<Option<Vec<u8>>>;
}

/// An open connection, ready to hand to a transport.
pub struct SocketPair {
    pub sender: Box<dyn SocketSender>,
    pub receiver: Box<dyn SocketReceiver>,
}

impl SocketPair {
    pub fn new(sender: impl SocketSender, receiver: impl SocketReceiver) -> Self {
        Self {
            sender: Box::new(sender),
            receiver: Box::new(receiver),
        }
    }
}
