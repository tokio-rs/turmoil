//! This module contains the simulated TCP/UDP networking types.
//!
//! They mirror [tokio::net](https://docs.rs/tokio/latest/tokio/net/) to provide
//! a high fidelity implementation.

mod listener;
pub use listener::TcpListener;

mod stream;
pub use stream::TcpStream;

mod udp;
pub use udp::UdpSocket;
