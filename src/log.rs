use crate::{version, Message};

use std::fs::File;
use std::io::prelude::*;
use std::io::BufWriter;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

pub(crate) struct Log {
    writer: Option<BufWriter<File>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct Event {
    /// Host & version at which the event originated.
    host: version::Dot,

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
        src: version::Dot,
    },
    /// The host sent a message
    Send {
        /// where the message is being sent
        dst: SocketAddr,
    },
}

impl Log {
    /// Log to the file
    pub(crate) fn new(path: &Path) -> Log {
        let file = File::create(path).unwrap();

        Log {
            writer: Some(BufWriter::new(file)),
        }
    }

    /// Don't log
    pub(crate) fn none() -> Log {
        Log { writer: None }
    }

    pub(crate) fn recv(
        &mut self,
        host: version::Dot,
        elapsed: Duration,
        src: version::Dot,
        message: &dyn Message,
    ) {
        if let Some(writer) = &mut self.writer {
            serde_json::to_writer_pretty(
                &mut *writer,
                &Event {
                    host,
                    elapsed,
                    kind: Kind::Recv { src },
                },
            )
            .unwrap();
            write!(writer, "\n").unwrap();
            message.write_json(writer);
            write!(writer, "\n").unwrap();
        }
    }

    pub(crate) fn send(
        &mut self,
        host: version::Dot,
        elapsed: Duration,
        dst: SocketAddr,
        message: &dyn Message,
    ) {
        if let Some(writer) = &mut self.writer {
            serde_json::to_writer_pretty(
                &mut *writer,
                &Event {
                    host,
                    elapsed,
                    kind: Kind::Send { dst },
                },
            )
            .unwrap();
            write!(writer, "\n").unwrap();
            message.write_json(writer);
            write!(writer, "\n").unwrap();
        }
    }
}
