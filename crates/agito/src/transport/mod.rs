//! Protocol transports: sockets, the CDP connection, sessions and capabilities.

pub mod capabilities;
pub mod cdp;
pub mod events;
pub mod log;
pub mod pipe;
pub mod session;
pub mod socket;
pub mod ws;

pub use capabilities::Capabilities;
pub use cdp::{CdpConnection, ConnectionOptions, ConnectionState, TransportError};
pub use events::{EventStream, RawEvent, TypedEventStream, event_key};
pub use log::ProtocolLogOptions;
pub use session::{CdpSession, CdpSessionRegistry, Session, SessionRegistry};
pub use socket::{SocketPair, SocketReceiver, SocketSender};
