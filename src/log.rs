use crate::{version, Dns, Message};

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
struct Event<'a> {
    /// Host & version at which the event originated.
    host: Dot<'a>,

    /// How much time, from the point of view of the host, has elapsed since the start of the simulation.
    elapsed: Duration,

    /// Event kind
    kind: Kind<'a>,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind<'a> {
    /// The host received a message
    Recv {
        /// Host & version at which the message originated
        src: Dot<'a>,
    },
    /// The host sent a message
    Send {
        /// where the message is being sent
        dst: &'a str,

        /// How long the message will be delayed before the peer receives it.
        delay: Option<Duration>,

        /// If true, the message is dropped.
        dropped: bool,
    },
    Log {
        level: usize,
        line: &'a str,
    },
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct Dot<'a> {
    host: &'a str,
    version: u64,
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

    pub(crate) fn enabled(&self) -> bool {
        self.writer.is_some()
    }

    pub(crate) fn line(
        &mut self,
        dns: &Dns,
        host: version::Dot,
        elapsed: Duration,
        debug: bool,
        line: &str,
    ) {
        if let Some(writer) = &mut self.writer {
            serde_json::to_writer_pretty(
                &mut *writer,
                &Event {
                    host: Dot::from(host, dns),
                    elapsed,
                    kind: Kind::Log {
                        level: if debug { 0 } else { 1 },
                        line,
                    },
                },
            )
            .unwrap();
            write!(writer, "\n").unwrap();
        }
    }

    pub(crate) fn recv(
        &mut self,
        dns: &Dns,
        host: version::Dot,
        elapsed: Duration,
        src: version::Dot,
        message: &dyn Message,
    ) {
        if let Some(writer) = &mut self.writer {
            serde_json::to_writer_pretty(
                &mut *writer,
                &Event {
                    host: Dot::from(host, dns),
                    elapsed,
                    kind: Kind::Recv {
                        src: Dot::from(src, dns),
                    },
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
        dns: &Dns,
        host: version::Dot,
        elapsed: Duration,
        dst: SocketAddr,
        delay: Option<Duration>,
        dropped: bool,
        message: &dyn Message,
    ) {
        if let Some(writer) = &mut self.writer {
            serde_json::to_writer_pretty(
                &mut *writer,
                &Event {
                    host: Dot::from(host, dns),
                    elapsed,
                    kind: Kind::Send {
                        dst: dns.reverse(dst),
                        delay,
                        dropped,
                    },
                },
            )
            .unwrap();
            write!(writer, "\n").unwrap();
            message.write_json(writer);
            write!(writer, "\n").unwrap();
        }
    }
}

impl<'a> Dot<'a> {
    fn from(dot: version::Dot, dns: &'a Dns) -> Dot<'a> {
        Dot {
            host: dns.reverse(dot.host),
            version: dot.version,
        }
    }
}
