//! This module contains the simulated TCP/UDP networking types.
//!
//! They mirror [tokio::net](https://docs.rs/tokio/latest/tokio/net/) to provide
//! a high fidelity implementation.

pub mod tcp;
pub use tcp::{listener::TcpListener, stream::TcpStream};

mod udp;
pub use udp::UdpSocket;

mod socketpair;
pub(crate) use socketpair::SocketPair;
