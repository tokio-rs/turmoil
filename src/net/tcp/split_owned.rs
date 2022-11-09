use std::{
    error::Error,
    fmt, io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::net::TcpStream;

use super::stream::{ReadHalf, WriteHalf};

/// Owned read half of a `TcpStream`, created by `into_split`.
#[derive(Debug)]
pub struct OwnedReadHalf {
    pub(crate) inner: ReadHalf,
}

impl OwnedReadHalf {
    /// Returns the local address that this stream is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.inner.pair.local)
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.inner.pair.remote)
    }

    /// Attempts to put the two halves of a `TcpStream` back together and
    /// recover the original socket. Succeeds only if the two halves
    /// originated from the same call to `into_split`.
    pub fn reunite(self, other: OwnedWriteHalf) -> Result<TcpStream, ReuniteError> {
        reunite(self, other)
    }
}

/// Owned write half of a `TcpStream`, created by `into_split`.
///
/// Note that in the [`AsyncWrite`] implementation of this type, [`poll_shutdown`] will
/// shut down the TCP stream in the write direction. Dropping the write half
/// will also shut down the write half of the TCP stream.
///
/// [`AsyncWrite`]: trait@tokio::io::AsyncWrite
/// [`poll_shutdown`]: fn@tokio::io::AsyncWrite::poll_shutdown
#[derive(Debug)]
pub struct OwnedWriteHalf {
    pub(crate) inner: WriteHalf,
}

impl OwnedWriteHalf {
    /// Returns the local address that this stream is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.inner.pair.local)
    }

    /// Returns the remote address that this stream is connected to.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.inner.pair.remote)
    }

    /// Attempts to put the two halves of a `TcpStream` back together and
    /// recover the original socket. Succeeds only if the two halves
    /// originated from the same call to `into_split`.
    pub fn reunite(self, other: OwnedReadHalf) -> Result<TcpStream, ReuniteError> {
        reunite(other, self)
    }
}

fn reunite(read: OwnedReadHalf, write: OwnedWriteHalf) -> Result<TcpStream, ReuniteError> {
    if Arc::ptr_eq(&read.inner.pair, &write.inner.pair) {
        Ok(TcpStream::reunite(read.inner, write.inner))
    } else {
        Err(ReuniteError(read, write))
    }
}

/// Error indicating that two halves were not from the same socket, and thus could
/// not be reunited.
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

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
