//! Shared login and simulator transport, with independent viewer and chat clients.

#[cfg(any(feature = "viewer", feature = "chat"))]
#[path = "session/attach_queue.rs"]
mod attach_queue;
pub mod caps;
pub mod circuit;
mod circuit_socket;
pub mod control;
#[cfg(any(feature = "viewer", feature = "chat"))]
pub mod inventory;
pub mod login;
pub mod message_split;
#[cfg(any(feature = "viewer", feature = "chat"))]
mod object_header;
#[cfg(any(feature = "viewer", feature = "chat"))]
pub mod outfit_restore;
mod packet_loss;
pub mod probe;
mod sim_tls;
pub mod social;
pub mod stats;
#[cfg(test)]
mod test_http;
pub mod text_validation;
pub mod xmlrpc;

pub use login::{LoginRequest, LoginResponse, StartLocation};
pub use stats::{NetStats, NetStatsSnapshot};

#[cfg(feature = "chat")]
pub mod chat;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("http client: {0}")]
    Http(String),
}

pub(crate) fn simulator_tls() -> Result<rustls::ClientConfig, NetError> {
    sim_tls::client_config().map_err(|e| NetError::Http(format!("simulator TLS: {e}")))
}

/// Loopback test servers must not use the process's HTTP proxy (#3903).
#[cfg(test)]
pub(crate) fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().expect("HTTP client")
}

#[cfg(feature = "viewer")]
include!("viewer.rs");

/// Region identifier: global origin in meters packed as `(x << 32) | y`.
pub type RegionHandle = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryOfferStatus {
    /// The datagram was handed to the socket, possibly on a retry.
    Transmitted,
    /// The simulator acknowledged that datagram, not the resident's acceptance.
    Acknowledged,
}
