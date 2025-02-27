//! This module contains the simulated TCP/UDP networking types.
//!
//! They mirror [tokio::net](https://docs.rs/tokio/latest/tokio/net/) to provide
//! a high fidelity implementation.

pub mod parser;
pub use parser::*;

pub mod ip_addr;
pub use ip_addr::*;

pub mod socket_addr;
pub use socket_addr::*;

pub mod tcp;
pub use tcp::{listener::TcpListener, stream::TcpStream};

mod udp;
pub use udp::UdpSocket;

#[derive(Debug, Copy, Clone, Hash, Eq, PartialEq)]
pub(crate) struct SocketPair {
    pub(crate) local: SocketAddr,
    pub(crate) remote: SocketAddr,
}

impl SocketPair {
    pub(crate) fn new(local: SocketAddr, remote: SocketAddr) -> SocketPair {
        assert_ne!(local, remote);
        SocketPair { local, remote }
    }
}

impl std::fmt::Display for SocketPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}→{}", self.local, self.remote)
    }
}
