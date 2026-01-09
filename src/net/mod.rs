//! This module contains the simulated TCP/UDP networking types.
//!
//! They mirror [tokio::net](https://docs.rs/tokio/latest/tokio/net/) to provide
//! a high fidelity implementation.

use std::net::SocketAddr;
use std::{io, iter};

use crate::world::World;
use crate::ToSocketAddrs;

pub mod tcp;
pub use tcp::{listener::TcpListener, stream::TcpStream};

pub(crate) mod udp;
pub use udp::UdpSocket;

/// Performs a DNS resolution.
///
/// Must be called from within a turmoil simulation context.
pub async fn lookup_host<T>(host: T) -> io::Result<impl Iterator<Item = SocketAddr>>
where
    T: ToSocketAddrs,
{
    let addr = World::current(|world| host.to_socket_addr(&world.dns))?;
    Ok(iter::once(addr))
}

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
