use std::net::SocketAddr;

use bytes::Bytes;

#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) src: SocketAddr,
    pub(crate) dst: SocketAddr,
    pub(crate) message: Protocol,
}

#[derive(Debug)]
pub(crate) enum Protocol {
    Udp(Bytes),
}
