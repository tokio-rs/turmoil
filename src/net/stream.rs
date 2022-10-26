use std::{
    io::{self, Result},
    pin::Pin,
    task::{ready, Context, Poll},
};

use bytes::{Buf, Bytes};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{mpsc, oneshot},
};

use crate::{
    envelope::{Protocol, Segment, Syn},
    host::SequencedSegment,
    trace,
    world::World,
    ToSocketAddr,
};

use super::SocketPair;

/// A simulated TCP stream between a local and a remote socket.
///
/// All methods must be called from a host within a Turmoil simulation.
// TODO: implement split_into
pub struct TcpStream {
    pair: SocketPair,
    receiver: mpsc::Receiver<SequencedSegment>,
    /// EOF received
    is_closed: bool,
    /// FIN sent, closed for writes
    is_shutdown: bool,
}

impl TcpStream {
    pub(crate) fn new(pair: SocketPair, receiver: mpsc::Receiver<SequencedSegment>) -> Self {
        Self {
            pair,
            receiver,
            is_closed: false,
            is_shutdown: false,
        }
    }

    /// Opens a TCP connection to a remote host.
    pub async fn connect<A: ToSocketAddr>(addr: A) -> Result<TcpStream> {
        let (ack, syn_ack) = oneshot::channel();

        let (pair, rx) = World::current(|world| {
            let dst = addr.to_socket_addr(&world.dns);
            let syn = Segment::Syn(Syn { ack });

            let host = world.current_host_mut();
            let local_addr = (host.addr, host.assign_ephemeral_port()).into();

            let pair = SocketPair::new(local_addr, dst);
            let rx = host.tcp.new_stream(pair);
            world.send_message(local_addr, dst, Protocol::Tcp(syn));

            (pair, rx)
        });

        syn_ack.await.map_err(|_| {
            io::Error::new(io::ErrorKind::ConnectionRefused, pair.remote.to_string())
        })?;

        trace!(dst = ?pair.local, src = ?pair.remote, protocol = %"TCP SYN-ACK", "Recv");

        Ok(TcpStream::new(pair, rx))
    }

    fn poll_read_priv(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf) -> Poll<Result<()>> {
        if self.is_closed || buf.capacity() == 0 {
            return Poll::Ready(Ok(()));
        }

        match ready!(self.receiver.poll_recv(cx)) {
            Some(seg) => {
                trace!(dst = ?self.pair.local, src = ?self.pair.remote, protocol = %seg, "Recv");

                match seg {
                    SequencedSegment::Data(bytes) => {
                        buf.put_slice(bytes.as_ref());
                    }
                    SequencedSegment::Fin => {
                        self.is_closed = true;
                    }
                }

                Poll::Ready(Ok(()))
            }
            None => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "Connection reset",
            ))),
        }
    }

    fn poll_write_priv(&self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(0));
        }

        if self.is_shutdown {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Broken pipe",
            )));
        }

        let res = World::current(|world| {
            let bytes = Bytes::copy_from_slice(buf);
            let len = bytes.len();

            let seq = self.seq(world)?;
            self.send(world, Segment::Data(seq, bytes));

            Ok(len)
        });

        Poll::Ready(res)
    }

    fn poll_shutdown_priv(&mut self) -> Poll<Result<()>> {
        if self.is_shutdown {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Socket is not connected",
            )));
        }

        let res = World::current(|world| {
            let seq = self.seq(world)?;
            self.send(world, Segment::Fin(seq));

            self.is_shutdown = true;

            Ok(())
        });

        Poll::Ready(res)
    }

    // If a seq is not assignable the connection has been reset by the
    // peer.
    fn seq(&self, world: &mut World) -> Result<u64> {
        world
            .current_host_mut()
            .tcp
            .assing_send_seq(self.pair)
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe"))
    }

    fn send(&self, world: &mut World, segment: Segment) {
        world.send_message(self.pair.local, self.pair.remote, Protocol::Tcp(segment));
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        self.poll_read_priv(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        self.poll_write_priv(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.poll_shutdown_priv()
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            if let Some(seq) = world.current_host_mut().tcp.assing_send_seq(self.pair) {
                self.send(world, Segment::Fin(seq));
                world.current_host_mut().tcp.remove_stream(self.pair);
            }
        })
    }
}
