use std::{fmt::Display, net::SocketAddr};

use bytes::Bytes;

#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) src: SocketAddr,
    pub(crate) dst: SocketAddr,
    pub(crate) message: Protocol,
}

#[derive(Debug)]
pub(crate) enum Protocol {
    Udp(Datagram),
}

#[derive(Debug)]
pub(crate) struct Datagram(pub Bytes);

impl Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Udp(datagram) => Display::fmt(&datagram, f),
        }
    }
}

impl Display for Datagram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        udp_fmt(&self.0, f)
    }
}

fn udp_fmt(datagram: &Bytes, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "UDP [")?;

    for (i, &b) in datagram.iter().enumerate() {
        if i < datagram.len() - 1 {
            write!(f, "{:#2X}, ", b)?;
        } else {
            write!(f, "{:#2X}", b)?;
        }
    }

    write!(f, "]")
}
