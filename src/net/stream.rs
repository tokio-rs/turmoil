use bytes::Bytes;
use std::{
    io,
    pin::Pin,
    sync::Arc,
    task::{ready, Context, Poll},
};
use tokio_util::sync::ReusableBoxFuture;

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::Notify,
};

use crate::{world::World, ToSocketAddr};

use super::{Segment, SocketPair};

/// A simulated connection between two hosts.
///
/// All methods must be called from a host within a Turmoil simulation.
pub struct TcpStream {
    pub(crate) pair: SocketPair,
    notify: Arc<Notify>,
    read_fut: Option<ReusableBoxFuture<'static, ()>>,
    /// FIN received, closed for read
    is_closed: bool,
    /// Shutdown initiated, FIN sent, closed for write
    is_shutdown: bool,
}

impl TcpStream {
    pub(crate) fn new(pair: SocketPair, notify: Arc<Notify>) -> Self {
        Self {
            pair,
            notify,
            read_fut: None,
            is_closed: false,
            is_shutdown: false,
        }
    }

    /// Opens a connection to a remote host.
    ///
    /// `addr` is an address of the remote host. Anything which implements the
    /// [`ToSocketAddr`] trait can be supplied as the address.
    pub async fn connect(addr: impl ToSocketAddr) -> io::Result<Self> {
        let ((local, wait), dst) = World::current(|world| {
            let dst = world.lookup(addr);
            (world.connect(dst), dst)
        });

        let peer = wait
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::ConnectionRefused, dst.to_string()))?;

        let pair = SocketPair { local, peer };
        let notify = World::current(|world| world.current_host().subscribe(pair));

        Ok(Self::new(pair, notify))
    }

    fn poll_read_priv(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.is_closed || buf.capacity() == 0 {
            return Poll::Ready(Ok(()));
        }

        let read_fut = match self.read_fut.as_mut() {
            Some(fut) => fut,
            None => {
                // fast path if we can recv immediately
                let maybe = Self::recv(self.pair, buf);
                if let Some(res) = maybe {
                    self.is_closed = res == 0;
                    return Poll::Ready(Ok(()));
                }

                let notify = Arc::clone(&self.notify);
                self.read_fut
                    .get_or_insert(ReusableBoxFuture::new(
                        async move { notify.notified().await },
                    ))
            }
        };

        let _ = ready!(read_fut.poll(cx));

        // Loop until we recv a segment from the host or the read future
        // is pending. This is necessary as we might be notified, but
        // the segment is still "on the network" and we need to continue
        // polling.
        loop {
            let notify = Arc::clone(&self.notify);
            read_fut.set(async move { notify.notified().await });

            match Self::recv(self.pair, buf) {
                Some(res) => {
                    self.is_closed = res == 0;
                    return Poll::Ready(Ok(()));
                }
                _ => ready!(read_fut.poll(cx)),
            }
        }
    }

    fn recv(pair: SocketPair, buf: &mut ReadBuf<'_>) -> Option<usize> {
        match World::current(|world| world.recv_on(pair)) {
            Some(seg) => {
                let bytes = match seg {
                    Segment::Data(bytes) => bytes,
                    Segment::Fin => Bytes::new(), // EOF == 0 bytes
                };

                buf.put_slice(bytes.as_ref());
                return Some(bytes.len());
            }
            _ => None,
        }
    }

    fn poll_write_priv(&self, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        if buf.len() == 0 {
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
            world
                .embark_on(self.pair, Segment::Data(bytes))
                .then(|| buf.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "Broken pipe"))
        });

        Poll::Ready(res)
    }

    fn poll_shutdown_priv(&mut self) -> Poll<Result<(), io::Error>> {
        if self.is_shutdown {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Socket is not connected",
            )));
        }

        World::current(|world| {
            world.embark_on(self.pair, Segment::Fin);
            self.is_shutdown = true;
        });

        Poll::Ready(Ok(()))
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        World::current_if_set(|world| {
            world.embark_on(self.pair, Segment::Fin);
            world.current_host_mut().disconnect(self.pair)
        })
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.poll_write_priv(buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        self.poll_shutdown_priv()
    }
}
