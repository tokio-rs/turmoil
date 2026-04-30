//! Drop-in replacements for `tokio::net` types.

mod addr;
pub mod tcp;
mod udp;

pub use addr::ToSocketAddrs;
pub use tcp::listener::TcpListener;
pub use tcp::stream::TcpStream;
pub use udp::UdpSocket;
