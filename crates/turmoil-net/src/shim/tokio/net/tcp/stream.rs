//! Drop-in replacement for [`tokio::net::TcpStream`].

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::kernel::{Addr, Domain, Fd, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::tcp::split::{split, ReadHalf, WriteHalf};
use crate::shim::tokio::net::tcp::split_owned::{split_owned, OwnedReadHalf, OwnedWriteHalf};
use crate::shim::tokio::net::ToSocketAddrs;
use crate::sys;

#[derive(Debug)]
pub struct TcpStream {
    fd: Fd,
}

impl TcpStream {
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let peer = addr.to_socket_addr()?;
        // The fd is allocated before the handshake awaits, so anything
        // short of a successful return would leak it — cancellation
        // (timeout, select!) and handshake errors alike. FdGuard
        // handles both: it closes the fd on drop unless `disarm` is
        // called, which we only do after poll_connect returns Ok.
        let guard = FdGuard::new(sys(|k| k.open(domain_of(&peer), Type::Stream)));
        poll_fn(|cx| sys(|k| k.poll_connect(guard.fd, cx, &Addr::Inet(peer)))).await?;
        Ok(Self { fd: guard.disarm() })
    }

    pub(super) fn from_fd(fd: Fd) -> Self {
        Self { fd }
    }

    pub(super) fn fd(&self) -> Fd {
        self.fd
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match sys(|k| k.local_addr(self.fd))? {
            Addr::Inet(sa) => Ok(sa),
            Addr::Unix(_) => panic!("TcpStream is Addr::Inet"),
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match sys(|k| k.peer_addr(self.fd))? {
            Addr::Inet(sa) => Ok(sa),
            Addr::Unix(_) => panic!("TcpStream is Addr::Inet"),
        }
    }

    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match sys(|k| k.poll_recv(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        match sys(|k| k.poll_send(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        poll_fn(|cx| sys(|k| k.poll_peek(self.fd, cx, buf))).await
    }

    pub fn poll_peek(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        let fd = self.fd;
        let unfilled = buf.initialize_unfilled();
        match sys(|k| k.poll_peek(fd, cx, unfilled)) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub fn set_ttl(&self, ttl: u32) -> io::Result<()> {
        let ttl: u8 = ttl
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        sys(|k| k.set_option(self.fd, SocketOption::IpTtl(ttl)))
    }

    pub fn ttl(&self) -> io::Result<u32> {
        match sys(|k| k.get_option(self.fd, SocketOptionKind::IpTtl))? {
            SocketOption::IpTtl(v) => Ok(v as u32),
            _ => unreachable!(),
        }
    }

    /// Set `TCP_NODELAY`. Round-trips for API parity but has no
    /// effect on simulated traffic — we never model Nagle, so every
    /// write goes out at the next `egress` pass regardless. See the
    /// `TcpNoDelay` variant of `SocketOption` for the follow-up TODO.
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        sys(|k| k.set_option(self.fd, SocketOption::TcpNoDelay(nodelay)))
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        match sys(|k| k.get_option(self.fd, SocketOptionKind::TcpNoDelay))? {
            SocketOption::TcpNoDelay(v) => Ok(v),
            _ => unreachable!(),
        }
    }

    /// Splits a `TcpStream` into a borrowed read half and write half.
    /// More efficient than [`into_split`], but the halves can't move
    /// to independently spawned tasks.
    ///
    /// [`into_split`]: TcpStream::into_split
    #[allow(clippy::needless_lifetimes)]
    pub fn split<'a>(&'a mut self) -> (ReadHalf<'a>, WriteHalf<'a>) {
        split(self)
    }

    /// Splits a `TcpStream` into owned read and write halves, each
    /// movable across tasks. Dropping the write half shuts down the
    /// write side of the stream.
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        split_owned(self)
    }
}

pub(super) fn noop_cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        sys(|k| k.close(self.fd));
    }
}

// AsyncRead/AsyncWrite are implemented on `TcpStream` only, matching
// tokio. The `poll_*_priv` methods take `&self` so the split/borrowed
// halves can call them through their inner reference without needing
// `&TcpStream` trait impls.

impl TcpStream {
    pub(super) fn poll_read_priv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let fd = self.fd;
        let unfilled = buf.initialize_unfilled();
        match sys(|k| k.poll_recv(fd, cx, unfilled)) {
            Poll::Ready(Ok(n)) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub(super) fn poll_write_priv(
        &self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let fd = self.fd;
        sys(|k| k.poll_send(fd, cx, buf))
    }

    pub(super) fn poll_shutdown_priv(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fd = self.fd;
        sys(|k| k.poll_shutdown_write(fd, cx))
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.poll_read_priv(cx, buf)
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.poll_write_priv(cx, buf)
    }

    /// No-op — bytes are copied into the kernel's send buffer in
    /// `poll_write` and drained by `egress()`. There's no userspace
    /// buffer to flush.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_shutdown_priv(cx)
    }
}

fn domain_of(peer: &SocketAddr) -> Domain {
    match peer {
        SocketAddr::V4(_) => Domain::Inet,
        SocketAddr::V6(_) => Domain::Inet6,
    }
}

/// Closes an fd on drop unless disarmed. Used in shim paths that
/// allocate an fd before an `await` boundary — without it, a dropped
/// future (timeout, select!, panic) would leak the socket into the
/// kernel table.
struct FdGuard {
    fd: Fd,
    armed: bool,
}

impl FdGuard {
    fn new(fd: Fd) -> Self {
        Self { fd, armed: true }
    }

    fn disarm(mut self) -> Fd {
        self.armed = false;
        self.fd
    }
}

impl Drop for FdGuard {
    fn drop(&mut self) {
        if self.armed {
            sys(|k| k.close(self.fd));
        }
    }
}
