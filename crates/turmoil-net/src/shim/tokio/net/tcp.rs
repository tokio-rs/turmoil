//! Drop-in replacements for [`tokio::net::TcpListener`] and
//! [`tokio::net::TcpStream`].

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::kernel::{Addr, Domain, Fd, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::addr::sealed::Sealed;
use crate::shim::tokio::net::ToSocketAddrs;
use crate::sys;

pub struct TcpListener {
    fd: Fd,
}

impl TcpListener {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = Sealed::to_socket_addr(&addr)?;
        let fd = sys(|k| {
            let backlog = k.default_backlog;
            let fd = k.bind(&Addr::Inet(addr), Type::Stream)?;
            k.listen(fd, backlog)?;
            Ok::<_, io::Error>(fd)
        })?;
        Ok(Self { fd })
    }

    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    pub fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<(TcpStream, SocketAddr)>> {
        match sys(|k| k.poll_accept(self.fd, cx)) {
            Poll::Ready(Ok((fd, peer))) => Poll::Ready(Ok((TcpStream { fd }, peer))),
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match sys(|k| k.local_addr(self.fd))? {
            Addr::Inet(sa) => Ok(sa),
            Addr::Unix(_) => panic!("TcpListener is Addr::Inet"),
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
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        sys(|k| k.close(self.fd));
    }
}

pub struct TcpStream {
    fd: Fd,
}

impl TcpStream {
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let peer = Sealed::to_socket_addr(&addr)?;
        let fd = sys(|k| k.open(domain_of(&peer), Type::Stream));
        let res = poll_fn(|cx| sys(|k| k.poll_connect(fd, cx, &Addr::Inet(peer)))).await;
        if let Err(e) = res {
            sys(|k| k.close(fd));
            return Err(e);
        }
        Ok(Self { fd })
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

    /// Non-blocking read. Returns `WouldBlock` when `recv_buf` is
    /// empty and the connection is still open.
    pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match sys(|k| k.poll_recv(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    /// Non-blocking write. Returns `WouldBlock` when `send_buf` is at
    /// cap (the peer hasn't drained enough to make room).
    pub fn try_write(&self, buf: &[u8]) -> io::Result<usize> {
        match sys(|k| k.poll_send(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    /// Async peek — returns buffered bytes without draining `recv_buf`.
    /// Callers typically use this to sniff a protocol prefix (TLS
    /// ClientHello, HTTP/2 preface) without committing to a specific
    /// parser.
    pub async fn peek(&self, buf: &mut [u8]) -> io::Result<usize> {
        poll_fn(|cx| sys(|k| k.poll_peek(self.fd, cx, buf))).await
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
}

fn noop_cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        sys(|k| k.close(self.fd));
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
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
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let fd = self.fd;
        sys(|k| k.poll_send(fd, cx, buf))
    }

    /// No-op — bytes are copied into the kernel's send buffer in
    /// `poll_write` and drained by `egress()`. There's no userspace
    /// buffer to flush.
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let fd = self.fd;
        sys(|k| k.poll_shutdown_write(fd, cx))
    }
}

fn domain_of(peer: &SocketAddr) -> Domain {
    match peer {
        SocketAddr::V4(_) => Domain::Inet,
        SocketAddr::V6(_) => Domain::Inet6,
    }
}
