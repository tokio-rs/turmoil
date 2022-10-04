//! This module contains the simulated TCP/UDP networking types.
//!
//! They mirror [tokio::net](https://docs.rs/tokio/latest/tokio/net/) to provide
//! a high fidelity implementation.

use crate::{envelope::DeliveryInstructions, version::Dot, Message};

use tokio::sync::oneshot;

mod listener;

use bytes::Bytes;
pub use listener::TcpListener;

mod stream;
pub use stream::TcpStream;

/// Uniquely identifies a connection between two hosts.
///
/// Using `Dot` allows us to support multiple connections between the same two
/// hosts, as version bumps for every network operation, ie connect.
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq)]
pub(crate) struct SocketPair {
    pub(crate) local: Dot,
    pub(crate) peer: Dot,
}

impl SocketPair {
    pub(crate) fn flip(self) -> Self {
        Self {
            local: self.peer,
            peer: self.local,
        }
    }
}

/// Message used to initiate a new connection with a host.
#[derive(Debug)]
pub(crate) struct Syn {
    /// Notify the peer that the connection has been accepted.
    ///
    /// To connect, we only send one message (the SYN) and the rest (SYN-ACK and
    /// ACK) is instantaneous. The SYN message is subject to turmoil when a host
    /// connects, such as added delay, which sufficiently simulates the 3-step
    /// handshake.
    pub(crate) notify: oneshot::Sender<Dot>,
}

impl Message for Syn {
    fn write_json(&self, dst: &mut dyn std::io::Write) {
        write!(dst, "Syn").unwrap()
    }
}

/// Envelope for messages on an established connection.
pub(crate) struct StreamEnvelope {
    /// When (or if) to deliver the message
    pub(crate) instructions: DeliveryInstructions,

    /// Segment type
    pub(crate) segment: Segment,
}

/// Message types for established connections.
#[derive(Debug)]
pub(crate) enum Segment {
    Data(Bytes),
}

impl Message for Segment {
    fn write_json(&self, dst: &mut dyn std::io::Write) {
        match self {
            Segment::Data(_) => write!(dst, "Data").unwrap(),
        }
    }
}
