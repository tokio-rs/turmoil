use std::{fmt::Display, net::SocketAddr};

use bytes::Bytes;
use tokio::sync::oneshot;

#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) src: SocketAddr,
    pub(crate) dst: SocketAddr,
    pub(crate) message: Protocol,
}

#[derive(Debug)]
pub(crate) enum Protocol {
    Tcp(Segment),
    Udp(Datagram),
}

#[derive(Debug)]
pub(crate) struct Datagram(pub Bytes);

/// This is a simplification of real TCP.
///
/// We implement just enough to ensure fidelity and provide knobs for real world
/// scenarios, but we skip a ton of complexity (e.g. checksums, flow control,
/// etc) because said complexity isn't useful needed in tests.
#[derive(Debug)]
pub(crate) enum Segment {
    Syn(Syn),
    Data(u64, Bytes),
    Fin(u64),
    Rst,
}

#[derive(Debug)]
pub(crate) struct Syn {
    pub(crate) ack: oneshot::Sender<()>,
}

impl Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp(segment) => Display::fmt(segment, f),
            Protocol::Udp(datagram) => Display::fmt(&datagram, f),
        }
    }
}

impl Display for Datagram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        hex("UDP", &self.0, f)
    }
}

impl Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Segment::Syn(_) => write!(f, "TCP SYN"),
            Segment::Data(_, data) => hex("TCP", &data, f),
            Segment::Fin(_) => write!(f, "TCP FIN"),
            Segment::Rst => write!(f, "TCP RST"),
        }
    }
}

pub(crate) fn hex(
    protocol: &str,
    bytes: &Bytes,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    write!(f, "{} [", protocol)?;

    for (i, &b) in bytes.iter().enumerate() {
        if i < bytes.len() - 1 {
            write!(f, "{:#2X}, ", b)?;
        } else {
            write!(f, "{:#2X}", b)?;
        }
    }

    write!(f, "]")
}
