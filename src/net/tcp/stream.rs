use std::{
    fmt::Debug,
    io::{self, Result},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
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
    net::SocketPair,
    world::World,
    ToSocketAddrs, TRACING_TARGET,
};

use super::split_owned::{OwnedReadHalf, OwnedWriteHalf};

/// A simulated TCP stream between a local and a remote socket.
///
/// All methods must be called from a host within a Turmoil simulation.
#[derive(Debug)]
pub struct TcpStream {
    read_half: ReadHalf,
    write_half: WriteHalf,
}

impl TcpStream {
    pub(crate) fn new(pair: SocketPair, receiver: mpsc::Receiver<SequencedSegment>) -> Self {
        let pair = Arc::new(pair);
        let read_half = ReadHalf {
            pair: pair.clone(),
            receiver,
            is_closed: false,
        };

        let write_half = WriteHalf {
            pair,
            is_shutdown: false,
        };

        Self {
            read_half,
            write_half,
        }
    }

    /// Opens a TCP connection to a remote host.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<TcpStream> {
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

        tracing::trace!(target: TRACING_TARGET, dst = ?pair.local, src = ?pair.remote, protocol = %"TCP SYN-ACK", "Recv");

        Ok(TcpStream::new(pair, rx))
    }

    /// Returns the local address that this stream is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.read_half.pair.local)
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> Result<SocketAddr> {
        Ok(self.read_half.pair.remote)
    }

    pub(crate) fn reunite(read_half: ReadHalf, write_half: WriteHalf) -> Self {
        Self {
            read_half,
            write_half,
        }
    }

    /// Splits a `TcpStream` into a read half and a write half, which can be used
    /// to read and write the stream concurrently.
    ///
    /// **Note:** Dropping the write half will shut down the write half of the TCP
    /// stream. This is equivalent to calling [`shutdown()`] on the `TcpStream`.
    ///
    /// [`shutdown()`]: fn@tokio::io::AsyncWriteExt::shutdown
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        (
            OwnedReadHalf {
                inner: self.read_half,
            },
            OwnedWriteHalf {
                inner: self.write_half,
            },
        )
    }
}

pub(crate) struct ReadHalf {
    pub(crate) pair: Arc<SocketPair>,
    receiver: mpsc::Receiver<SequencedSegment>,
    /// FIN received, EOF for reades
    is_closed: bool,
}

impl ReadHalf {
    fn poll_read_priv(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf) -> Poll<Result<()>> {
        if self.is_closed || buf.capacity() == 0 {
            return Poll::Ready(Ok(()));
        }

        match ready!(self.receiver.poll_recv(cx)) {
            Some(seg) => {
                tracing::trace!(target: TRACING_TARGET, dst = ?self.pair.local, src = ?self.pair.remote, protocol = %seg, "Recv");

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
}

impl Debug for ReadHalf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadHalf")
            .field("pair", &self.pair)
            .field("is_closed", &self.is_closed)
            .finish()
    }
}

pub(crate) struct WriteHalf {
    pub(crate) pair: Arc<SocketPair>,
    /// FIN sent, closed for writes
    is_shutdown: bool,
}

impl WriteHalf {
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
            .assign_send_seq(*self.pair)
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe"))
    }

    fn send(&self, world: &mut World, segment: Segment) {
        world.send_message(self.pair.local, self.pair.remote, Protocol::Tcp(segment));
    }
}

impl Debug for WriteHalf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteHalf")
            .field("pair", &self.pair)
            .field("is_shutdown", &self.is_shutdown)
            .finish()
    }
}

impl AsyncRead for ReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        self.poll_read_priv(cx, buf)
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<Result<()>> {
        Pin::new(&mut self.read_half).poll_read(cx, buf)
    }
}

impl AsyncWrite for WriteHalf {
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

impl AsyncWrite for TcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        Pin::new(&mut self.write_half).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.write_half).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.write_half).poll_shutdown(cx)
    }
}

impl Drop for ReadHalf {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            world.current_host_mut().tcp.close_stream_half(*self.pair);
        })
    }
}

impl Drop for WriteHalf {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            let pair = *self.pair;

            if let Some(seq) = world.current_host_mut().tcp.assign_send_seq(pair) {
                self.send(world, Segment::Fin(seq));
                world.current_host_mut().tcp.close_stream_half(pair);
            }
        })
    }
}
