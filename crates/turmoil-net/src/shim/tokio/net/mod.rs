//! Drop-in replacements for `tokio::net` types.

mod addr;
mod tcp;
mod udp;

pub use addr::ToSocketAddrs;
pub use tcp::{TcpListener, TcpStream};
pub use udp::UdpSocket;
