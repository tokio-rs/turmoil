use crate::Message;

use std::cell::RefCell;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufWriter;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

pub(crate) struct Log {
    out: RefCell<BufWriter<File>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct Event {
    /// Host & version at which the event originated.
    host: Dot,

    /// How much time, from the point of view of the host, has elapsed since the start of the simulation.
    elapsed: Duration,

    /// Event kind
    kind: Kind,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    /// The host received a message
    Recv {
        /// Host & version at which the message originated
        src: Dot,
    },
    /// The host sent a message
    Send {
        /// where the message is being sent
        dst: SocketAddr,
    },
}

/// A host / version pair
#[derive(serde::Serialize)]
pub(crate) struct Dot {
    /// The host socket address
    pub(crate) host: SocketAddr,

    /// The version at which the event was emitted.
    pub(crate) version: u64,
}

impl Log {
    pub(crate) fn new(path: &Path) -> Log {
        let file = File::create(path).unwrap();

        Log {
            out: RefCell::new(BufWriter::new(file)),
        }
    }

    pub(crate) fn recv(
        &self,
        host: Dot,
        elapsed: Duration,
        src: Dot,
        message: &dyn Message,
    ) {
        let mut out = self.out.borrow_mut();
        serde_json::to_writer_pretty(
            &mut *out,
            &Event {
                host,
                elapsed,
                kind: Kind::Recv { src }
            }
        )
        .unwrap();
        write!(out, "\n").unwrap();
        message.write_json(&mut *out);
        write!(out, "\n").unwrap();
    }

    pub(crate) fn send(
        &self,
        host: Dot,
        elapsed: Duration,
        dst: SocketAddr,
        message: &dyn Message,
    ) {
        let mut out = self.out.borrow_mut();
        serde_json::to_writer_pretty(
            &mut *out,
            &Event {
                host,
                elapsed,
                kind: Kind::Send { dst },
            }
        )
        .unwrap();
        write!(out, "\n").unwrap();
        message.write_json(&mut *out);
        write!(out, "\n").unwrap();
    }
}
