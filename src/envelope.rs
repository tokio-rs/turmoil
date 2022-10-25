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

/// This is a parody of TCP.
///
/// We implement just enough to make it realistic, but we skip a ton of
/// complexity (e.g. checksums, flow control, etc).
#[derive(Debug)]
pub(crate) enum Segment {
    Syn(Syn),
    Data(u64, StreamData),
    Fin(u64),
    Rst,
}

#[derive(Debug)]
pub(crate) struct StreamData(pub Bytes);

impl StreamData {
    pub(crate) fn eof() -> Self {
        Self(Bytes::new())
    }
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
        write!(f, "UDP ")?;
        hex(&self.0, f)
    }
}

impl Display for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Segment::Syn(_) => write!(f, "TCP SYN"),
            Segment::Data(_, data) => Display::fmt(&data, f),
            Segment::Fin(_) => write!(f, "TCP FIN"),
            Segment::Rst => write!(f, "TCP RST"),
        }
    }
}

impl Display for StreamData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            write!(f, "FIN")
        } else {
            write!(f, "TCP ")?;
            hex(&self.0, f)
        }
    }
}

fn hex(bytes: &Bytes, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "[")?;

    for (i, &b) in bytes.iter().enumerate() {
        if i < bytes.len() - 1 {
            write!(f, "{:#2X}, ", b)?;
        } else {
            write!(f, "{:#2X}", b)?;
        }
    }

    write!(f, "]")
}
