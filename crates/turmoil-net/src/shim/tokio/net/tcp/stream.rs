//! Drop-in replacement for [`tokio::net::TcpStream`].

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf, Ready};

use crate::kernel::{Addr, Domain, Fd, Readiness, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::tcp::split::{split, ReadHalf, WriteHalf};
use crate::shim::tokio::net::tcp::split_owned::{split_owned, OwnedReadHalf, OwnedWriteHalf};
use crate::shim::tokio::net::tcp::wait::{Direction, Waiter};
use crate::shim::tokio::net::ToSocketAddrs;
use crate::{sys, sys_if_installed};

#[derive(Debug)]
pub struct TcpStream {
    fd: Fd,
    read: Direction,
    write: Direction,
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
        Ok(Self::from_fd(guard.disarm()))
    }

    pub(super) fn from_fd(fd: Fd) -> Self {
        Self {
            fd,
            read: Direction::new(),
            write: Direction::new(),
        }
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

    /// Read available data into the buffers in order without waiting.
    ///
    /// Returns the total bytes read, which may fill only part of the buffers,
    /// or `WouldBlock` if no data is available. `Ok(0)` indicates EOF or that
    /// all supplied buffers are empty.
    pub fn try_read_vectored(&self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        sys(|k| k.try_recv_vectored(self.fd, bufs))
    }

    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        match sys(|k| k.poll_send(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        let waiter = self.read.waiter();
        poll_fn(|cx| {
            waiter.set(cx.waker());
            let mut fan_out = Context::from_waker(self.read.waker());
            sys(|k| k.poll_peek(self.fd, &mut fan_out, buf))
        })
        .await
    }

    pub fn poll_peek(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<usize>> {
        let fd = self.fd;
        self.read.set_poll(cx.waker());
        let mut fan_out = Context::from_waker(self.read.waker());
        let unfilled = buf.initialize_unfilled();
        match sys(|k| k.poll_peek(fd, &mut fan_out, unfilled)) {
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

    /// Waits for any of the requested ready states. Like tokio, the
    /// result only reports states in `interest`, and an aborted
    /// connection is ready in every direction (the next operation
    /// reports the error).
    pub async fn ready(&self, interest: Interest) -> io::Result<Ready> {
        // Errors arrive through the same wakeups as reads.
        let read = (interest.is_readable() || interest.is_error()).then(|| self.read.waiter());
        let write = interest.is_writable().then(|| self.write.waiter());
        poll_fn(|cx| self.poll_ready_waiters(interest, cx, read.as_ref(), write.as_ref())).await
    }

    /// Waits until a read would not block: data, EOF, or an error.
    pub async fn readable(&self) -> io::Result<()> {
        self.ready(Interest::READABLE).await.map(|_| ())
    }

    /// Waits until a write would not block: buffer space, or an error.
    pub async fn writable(&self) -> io::Result<()> {
        self.ready(Interest::WRITABLE).await.map(|_| ())
    }

    /// Polls for read readiness. Only the waker from the most recent
    /// call is woken, as with tokio's `poll_*` methods.
    pub fn poll_read_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.read.set_poll(cx.waker());
        self.poll_ready(Interest::READABLE).map_ok(|_| ())
    }

    /// Polls for write readiness. Only the waker from the most recent
    /// call is woken, as with tokio's `poll_*` methods.
    pub fn poll_write_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.write.set_poll(cx.waker());
        self.poll_ready(Interest::WRITABLE).map_ok(|_| ())
    }

    fn poll_ready_waiters(
        &self,
        interest: Interest,
        cx: &mut Context<'_>,
        read: Option<&Waiter<'_>>,
        write: Option<&Waiter<'_>>,
    ) -> Poll<io::Result<Ready>> {
        for waiter in [read, write].into_iter().flatten() {
            waiter.set(cx.waker());
        }
        self.poll_ready(interest)
    }

    /// Report readiness in `interest`, or park the fan-out wakers for
    /// the directions it covers.
    fn poll_ready(&self, interest: Interest) -> Poll<io::Result<Ready>> {
        let ready = sys(|k| {
            let ready = to_ready(k.readiness(self.fd)?, interest);
            if ready.is_empty() {
                if interest.is_readable() || interest.is_error() {
                    k.register_read_waker(self.fd, self.read.waker())?;
                }
                if interest.is_writable() {
                    k.register_write_waker(self.fd, self.write.waker())?;
                }
            }
            Ok(ready)
        });
        match ready {
            Ok(ready) if ready.is_empty() => Poll::Pending,
            ready => Poll::Ready(ready),
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

/// Map kernel readiness onto tokio's, restricted to `interest`.
/// Priority (out-of-band) data is never modeled.
pub(crate) fn to_ready(r: Readiness, interest: Interest) -> Ready {
    let mut ready = Ready::EMPTY;
    if interest.is_readable() {
        if r.readable {
            ready |= Ready::READABLE;
        }
        if r.read_closed {
            ready |= Ready::READ_CLOSED;
        }
    }
    if interest.is_writable() {
        if r.writable {
            ready |= Ready::WRITABLE;
        }
        if r.write_closed {
            ready |= Ready::WRITE_CLOSED;
        }
    }
    if interest.is_error() && r.error {
        ready |= Ready::ERROR;
    }
    ready
}

pub(super) fn noop_cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        sys_if_installed(|k| k.close(self.fd));
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
        self.read.set_poll(cx.waker());
        let mut fan_out = Context::from_waker(self.read.waker());
        let unfilled = buf.initialize_unfilled();
        match sys(|k| k.poll_recv(fd, &mut fan_out, unfilled)) {
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
        self.write.set_poll(cx.waker());
        let mut fan_out = Context::from_waker(self.write.waker());
        sys(|k| k.poll_send(fd, &mut fan_out, buf))
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
            sys_if_installed(|k| k.close(self.fd));
        }
    }
}
