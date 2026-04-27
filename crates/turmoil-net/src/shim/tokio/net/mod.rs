//! Drop-in replacements for `tokio::net` types.

mod addr;
mod udp;

pub use addr::ToSocketAddrs;
pub use udp::UdpSocket;
