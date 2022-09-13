use crate::{io::*, *};

use std::{fmt::Display, io, net::SocketAddr};

use futures::FutureExt;
use indexmap::IndexMap;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

/// A connection between two hosts.
pub struct Connection<M> {
    /// Connection identification information
    info: ConnectionInfo,

    /// Message receiver
    rx: mpsc::Receiver<M>,

    /// Message sender; ConnectionInfo is included for routing
    tx: mpsc::Sender<(ConnectionInfo, M)>,
}

impl<M> Connection<M> {
    /// Send a message.
    ///
    /// # Errors
    ///
    /// If the connection is reset by the peer an I/O error will be returned.
    pub async fn send(&self, message: M) -> io::Result<()> {
        if let Err(_) = self.tx.send((self.info, message)).await {
            return Err(io::Error::from(io::ErrorKind::ConnectionReset));
        }

        Ok(())
    }

    /// Receive a message.
    ///
    /// # Errors
    ///
    /// If the connection is reset by the peer an I/O error will be returned.
    pub async fn recv(&mut self) -> io::Result<M> {
        self.rx
            .recv()
            .await
            .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionReset))
    }
}

#[derive(Debug)]
pub enum Segment<M: Message> {
    /// Initiate a new connection
    Syn(u64),

    /// Accept a new connection
    SynAck(u64),

    /// Send a message on an established connection
    Data(u64, M),

    /// Connection reset
    Rst(u64),
}

impl<M: Message> Message for Segment<M> {
    fn write_json(&self, dst: &mut dyn std::io::Write) {
        match self {
            Segment::Syn(_) => write!(dst, "Syn").unwrap(),
            Segment::SynAck(_) => write!(dst, "SynAck").unwrap(),
            Segment::Data(_, m) => m.write_json(dst),
            Segment::Rst(_) => write!(dst, "Rst").unwrap(),
        };
    }
}

/// Layers distinct connections over a host's [`Io`].
pub struct ConnectionIo<M: Message> {
    /// Accept queue
    queue: mpsc::Receiver<Connection<M>>,

    /// Action sender
    sender: mpsc::Sender<Action<M>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ConnectionInfo {
    /// A unique identifier for each connection
    id: u64,

    /// Address of the peer on the other end
    peer: SocketAddr,
}

enum Action<M> {
    /// Accept a new connection
    Accept(ConnectionInfo),

    /// Initiate a new connection
    Connect {
        dst: SocketAddr,
        notify: oneshot::Sender<Connection<M>>,
    },
}

impl<M: Message> ConnectionIo<M> {
    pub fn new() -> Self {
        let (action_tx, action_rx) = mpsc::channel(1);
        let (accept_tx, accept_rx) = mpsc::channel(1);

        let inner = Inner {
            ctr: 0,
            connections: Connections::new(),
            queue: accept_tx,
            receiver: action_rx,
        };

        tokio::task::spawn_local(Self::event_loop(inner));

        Self {
            queue: accept_rx,
            sender: action_tx,
        }
    }

    /// Accepts a new incoming connection.
    pub async fn accept(&mut self) -> Option<Connection<M>> {
        match self.queue.recv().await {
            Some(c) => {
                let _ = self.sender.send(Action::Accept(c.info.clone())).await;
                Some(c)
            }
            None => None,
        }
    }

    /// Opens a connection to a peer.
    pub async fn connect(&self, to: impl ToSocketAddr) -> io::Result<Connection<M>> {
        let dst = World::current(|world| world.lookup(to));

        let (tx, rx) = oneshot::channel();
        let _ = self.sender.send(Action::Connect { dst, notify: tx }).await;

        rx.await
            .map_err(|_| io::Error::from(io::ErrorKind::ConnectionRefused))
    }

    async fn event_loop(mut inner: Inner<M>) {
        loop {
            futures::select_biased! {
                maybe_action = inner.receiver.recv().fuse() => match maybe_action {
                    Some(action) => match action {
                        Action::Accept(info) => inner.accept(info),
                        Action::Connect { dst, notify } => inner.connect(dst, notify),
                    },
                    None => break,
                },
                recv = inner.connections.receivers.next() => {
                    match recv {
                        Some((idx, maybe_msg)) => match maybe_msg {
                            Some((info, msg)) => inner.send(info, msg),
                            None => inner.rst(idx),
                        },
                        None => continue,
                    }
                },
                (seg, src) = recv().fuse() => inner.route(src, seg).await,
            }
        }

        inner.rst_all();
    }
}

struct Inner<M: Message> {
    /// Dense counter for connection identifiers
    ctr: u64,

    /// Active connections
    connections: Connections<M>,

    /// Accept queue
    queue: mpsc::Sender<Connection<M>>,

    /// Action receiver
    receiver: mpsc::Receiver<Action<M>>,
}

impl<M: Message> Inner<M> {
    fn accept(&mut self, info: ConnectionInfo) {
        self.connections.accept(info);
        send::<Segment<M>>(info.peer, Segment::SynAck(info.id));
    }

    fn connect(&mut self, dst: SocketAddr, notify: oneshot::Sender<Connection<M>>) {
        self.ctr += 1;
        let id = self.ctr;

        self.connections.connect(id, dst, notify);
        send::<Segment<M>>(dst, Segment::Syn(id));
    }

    fn send(&self, info: ConnectionInfo, message: M) {
        self.connections.check_connected(&info);

        let ConnectionInfo { id, peer } = info;
        send::<Segment<M>>(peer, Segment::Data(id, message))
    }

    async fn route(&mut self, from: SocketAddr, seg: Segment<M>) {
        match seg {
            Segment::Syn(id) => {
                let connection = self
                    .connections
                    .register_for_accept(ConnectionInfo { id, peer: from });
                let _ = self.queue.send(connection).await;
            }
            Segment::SynAck(id) => self
                .connections
                .finish_connect(ConnectionInfo { id, peer: from }),
            Segment::Data(id, message) => {
                self.connections
                    .recv(ConnectionInfo { id, peer: from }, message)
                    .await
            }
            Segment::Rst(id) => {
                let _ = self
                    .connections
                    .disconnect(ConnectionInfo { id, peer: from });
            }
        }
    }

    fn rst(&mut self, idx: usize) {
        if let Some(info) = self.connections.deregister(idx) {
            send::<Segment<M>>(info.peer, Segment::Rst(info.id))
        }
    }

    fn rst_all(&mut self) {
        let Connections { by_info, .. } = &self.connections;

        by_info.iter().for_each(|(info, s)| {
            if let ConnectionState::Queued { .. } | ConnectionState::Connected { .. } = s {
                send::<Segment<M>>(info.peer, Segment::Rst(info.id))
            }
        });
    }
}

/// Connection state management.
struct Connections<M> {
    // State for each connection
    by_info: IndexMap<ConnectionInfo, ConnectionState<M>>,

    /// Receiver index to ConnectionInfo
    by_idx: IndexMap<usize, ConnectionInfo>,

    /// Connection message receivers
    receivers: unicycle::IndexedStreamsUnordered<ReceiverStream<(ConnectionInfo, M)>>,
}

enum ConnectionState<M> {
    /// Connection initiated; awaiting syn-ack
    New {
        recv_idx: usize,
        sender: mpsc::Sender<(ConnectionInfo, M)>,
        notify: oneshot::Sender<Connection<M>>,
    },

    /// Syn received; awaiting accept
    Queued {
        sender: mpsc::Sender<M>,
        receiver: mpsc::Receiver<(ConnectionInfo, M)>,
    },

    /// Connected! Data segments may flow in both directions
    Connected {
        recv_idx: usize,
        sender: mpsc::Sender<M>,
    },
}

impl<M> Display for ConnectionState<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionState::New { .. } => write!(f, "New"),
            ConnectionState::Queued { .. } => write!(f, "Queued"),
            ConnectionState::Connected { .. } => write!(f, "Connected"),
        }
    }
}

impl<M> Connections<M> {
    fn new() -> Self {
        Self {
            by_info: IndexMap::new(),
            by_idx: IndexMap::new(),
            receivers: unicycle::IndexedStreamsUnordered::new(),
        }
    }

    fn check_connected(&self, info: &ConnectionInfo) {
        if let ConnectionState::New { .. } | ConnectionState::Queued { .. } = self.by_info[info] {
            panic!("not connected: {:?}", info)
        }
    }

    fn accept(&mut self, info: ConnectionInfo) {
        let (tx, rx) = match self
            .by_info
            .remove(&info)
            .expect(&format!("not registered: {:?}", info))
        {
            ConnectionState::Queued { sender, receiver } => (sender, receiver),
            other => panic!("invalid state: {}, {:?}", other, info),
        };

        let idx = self.receivers.push(ReceiverStream::new(rx));
        self.by_idx.insert(idx, info);

        self.by_info.insert(
            info,
            ConnectionState::Connected {
                recv_idx: idx,
                sender: tx,
            },
        );
    }

    fn register_for_accept(&mut self, info: ConnectionInfo) -> Connection<M> {
        if self.by_info.contains_key(&info) {
            panic!("already registered: {:?}", info);
        }

        let (send_tx, send_rx) = mpsc::channel(1);
        let (recv_tx, recv_rx) = mpsc::channel(1);

        let queued = ConnectionState::Queued {
            sender: recv_tx,
            receiver: send_rx,
        };

        self.by_info.insert(info, queued);

        Connection {
            info,
            rx: recv_rx,
            tx: send_tx,
        }
    }

    fn connect(&mut self, id: u64, dst: SocketAddr, notify: oneshot::Sender<Connection<M>>) {
        let info = ConnectionInfo { id, peer: dst };

        let (tx, rx) = mpsc::channel(1);
        let idx = self.receivers.push(ReceiverStream::new(rx));
        let new = ConnectionState::New {
            recv_idx: idx,
            sender: tx,
            notify,
        };

        self.by_idx.insert(idx, info);
        self.by_info.insert(info, new);
    }

    fn finish_connect(&mut self, info: ConnectionInfo) {
        let (recv_idx, send_tx, notify) = match self
            .by_info
            .remove(&info)
            .expect(&format!("not registered: {:?}", info))
        {
            ConnectionState::New {
                recv_idx,
                sender,
                notify,
            } => (recv_idx, sender, notify),
            other => panic!("invalid state: {}, {:?}", other, info),
        };

        let (recv_tx, recv_rx) = mpsc::channel(1);
        let connection = Connection {
            info,
            rx: recv_rx,
            tx: send_tx,
        };

        self.by_info.insert(
            info,
            ConnectionState::Connected {
                recv_idx,
                sender: recv_tx,
            },
        );

        let _ = notify.send(connection);
    }

    async fn recv(&self, info: ConnectionInfo, message: M) {
        match &self.by_info[&info] {
            ConnectionState::Connected { sender, .. } => {
                let _ = sender.send(message).await;
            }
            other => panic!("invalid state: {}, {:?}", other, info),
        }
    }

    fn deregister(&mut self, idx: usize) -> Option<ConnectionInfo> {
        let info = self.by_idx.remove(&idx)?;
        self.by_info.remove(&info)?;

        Some(info)
    }

    fn disconnect(&mut self, info: ConnectionInfo) -> Option<()> {
        let idx = match self.by_info.remove(&info)? {
            ConnectionState::New { recv_idx, .. } => Some(recv_idx),
            ConnectionState::Connected { recv_idx, .. } => Some(recv_idx),
            _ => None,
        }?;

        let recv = self.receivers.get_mut(idx)?;
        recv.close();

        Some(())
    }
}
