//! Drop-in replacements for `tokio::net` types.

mod addr;
pub mod tcp;
mod udp;

pub use addr::ToSocketAddrs;
pub use tcp::listener::TcpListener;
pub use tcp::stream::TcpStream;
pub use udp::UdpSocket;

use std::io;
use std::net::SocketAddr;

/// Resolve `host` against the installed `Net`'s DNS. Mirrors
/// [`tokio::net::lookup_host`]; each name maps to one address.
///
/// [`tokio::net::lookup_host`]: https://docs.rs/tokio/latest/tokio/net/fn.lookup_host.html
pub async fn lookup_host<T: ToSocketAddrs>(
    host: T,
) -> io::Result<impl Iterator<Item = SocketAddr>> {
    host.to_socket_addr().map(std::iter::once)
}
