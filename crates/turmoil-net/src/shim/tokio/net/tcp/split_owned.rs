//! `TcpStream` owned split support.
//!
//! [`OwnedReadHalf`] and [`OwnedWriteHalf`] are the owned variants of
//! [`ReadHalf`]/[`WriteHalf`] — they move independently across task
//! boundaries at the cost of an `Arc` allocation. Created via
//! [`TcpStream::into_split`].
//!
//! [`ReadHalf`]: super::ReadHalf
//! [`WriteHalf`]: super::WriteHalf
//! [`TcpStream::into_split`]: super::TcpStream::into_split

use std::error::Error;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::shim::tokio::net::tcp::stream::{noop_cx, TcpStream};
use crate::sys;

/// Owned read half of a [`TcpStream`], created by [`TcpStream::into_split`].
///
/// [`TcpStream::into_split`]: super::TcpStream::into_split
#[derive(Debug)]
pub struct OwnedReadHalf {
    inner: Arc<TcpStream>,
}

/// Owned write half of a [`TcpStream`], created by [`TcpStream::into_split`].
///
/// Dropping the write half shuts down the stream in the write
/// direction — the peer observes EOF even if other halves remain
/// alive.
///
/// [`TcpStream::into_split`]: super::TcpStream::into_split
#[derive(Debug)]
pub struct OwnedWriteHalf {
    inner: Arc<TcpStream>,
    shutdown_on_drop: bool,
}

pub(crate) fn split_owned(stream: TcpStream) -> (OwnedReadHalf, OwnedWriteHalf) {
    let arc = Arc::new(stream);
    (
        OwnedReadHalf { inner: arc.clone() },
        OwnedWriteHalf {
            inner: arc,
            shutdown_on_drop: true,
        },
    )
}

impl OwnedReadHalf {
    /// Attempt to reunite the halves into the original `TcpStream`.
    /// Succeeds only if both halves came from the same `into_split`
    /// call.
    pub fn reunite(self, other: OwnedWriteHalf) -> Result<TcpStream, ReuniteError> {
        reunite(self, other)
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.try_read(buf)
    }

    pub async fn peek(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.peek(buf).await
    }

    pub fn poll_peek(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_peek(cx, buf)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer_addr()
    }
}

impl OwnedWriteHalf {
    /// Attempt to reunite the halves into the original `TcpStream`.
    /// Succeeds only if both halves came from the same `into_split`
    /// call.
    pub fn reunite(self, other: OwnedReadHalf) -> Result<TcpStream, ReuniteError> {
        reunite(other, self)
    }

    /// Destroy the write half *without* shutting down the write side
    /// on drop. After this the read half's peer will still see bytes
    /// flowing until the underlying `TcpStream` drops; tokio's
    /// `forget` semantics match.
    pub fn forget(mut self) {
        self.shutdown_on_drop = false;
        drop(self);
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        self.inner.try_write(buf)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer_addr()
    }
}

fn reunite(read: OwnedReadHalf, write: OwnedWriteHalf) -> Result<TcpStream, ReuniteError> {
    if Arc::ptr_eq(&read.inner, &write.inner) {
        // Drop the write half without triggering shutdown_on_drop —
        // we're putting the stream back together, not tearing it down.
        let mut write = write;
        write.shutdown_on_drop = false;
        drop(write);
        // Both halves came from the same Arc; one drop brought the
        // strong count to 1, so `try_unwrap` succeeds.
        Arc::try_unwrap(read.inner).map_err(|inner| {
            ReuniteError(
                OwnedReadHalf {
                    inner: inner.clone(),
                },
                OwnedWriteHalf {
                    inner,
                    shutdown_on_drop: true,
                },
            )
        })
    } else {
        Err(ReuniteError(
            read,
            OwnedWriteHalf {
                inner: write.inner.clone(),
                shutdown_on_drop: write.shutdown_on_drop,
            },
        ))
    }
}

/// Error returned by [`OwnedReadHalf::reunite`] / [`OwnedWriteHalf::reunite`]
/// when the halves came from different `TcpStream`s.
#[derive(Debug)]
pub struct ReuniteError(pub OwnedReadHalf, pub OwnedWriteHalf);

impl fmt::Display for ReuniteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tried to reunite halves that are not from the same socket"
        )
    }
}

impl Error for ReuniteError {}

impl Drop for OwnedWriteHalf {
    fn drop(&mut self) {
        if self.shutdown_on_drop {
            // Shutdown is synchronous in our kernel (queues FIN, returns
            // Ready immediately) — safe to drive with a noop waker.
            let fd = self.inner.fd();
            let _ = sys(|k| k.poll_shutdown_write(fd, &mut noop_cx()));
        }
    }
}

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.inner.poll_read_priv(cx, buf)
    }
}

impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_write_priv(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    /// Shut down the write side of the stream. Sets `shutdown_on_drop`
    /// to false on success so Drop doesn't double-shutdown.
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let res = self.inner.poll_shutdown_priv(cx);
        if let Poll::Ready(Ok(())) = &res {
            self.shutdown_on_drop = false;
        }
        res
    }
}

impl AsRef<TcpStream> for OwnedReadHalf {
    fn as_ref(&self) -> &TcpStream {
        &self.inner
    }
}

impl AsRef<TcpStream> for OwnedWriteHalf {
    fn as_ref(&self) -> &TcpStream {
        &self.inner
    }
}
