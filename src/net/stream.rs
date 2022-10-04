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
}

impl TcpStream {
    pub(crate) fn new(pair: SocketPair, notify: Arc<Notify>) -> Self {
        Self {
            pair,
            notify,
            read_fut: None,
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
        let notify = World::current(|world| world.current_host_mut().finish_connect(pair));

        Ok(Self::new(pair, notify))
    }

    fn poll_read_priv(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let read_fut = match self.read_fut.as_mut() {
            Some(fut) => fut,
            None => {
                // fast path if we can recv immediately
                let maybe = Self::recv(self.pair, buf);
                if let Some(res) = maybe {
                    return Poll::Ready(res);
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
                Some(res) => return Poll::Ready(res),
                _ => ready!(read_fut.poll(cx)),
            }
        }
    }

    fn recv(pair: SocketPair, buf: &mut ReadBuf<'_>) -> Option<io::Result<()>> {
        match World::current(|world| world.recv_on(pair)) {
            Some(seg) => match seg {
                Segment::Data(bytes) => {
                    buf.put_slice(bytes.as_ref());
                    return Some(Ok(()));
                }
            },
            _ => None,
        }
    }

    fn poll_write_priv(&self, buf: &[u8]) -> Poll<Result<usize, io::Error>> {
        World::current(|world| {
            let bytes = Bytes::copy_from_slice(buf);
            world.embark_on(self.pair, Segment::Data(bytes))
        });

        Poll::Ready(Ok(buf.len()))
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

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        unimplemented!("shutdown is not implemented yet")
    }
}
