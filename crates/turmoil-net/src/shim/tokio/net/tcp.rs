//! Drop-in replacements for [`tokio::net::TcpListener`] and
//! [`tokio::net::TcpStream`].
//!
//! `TcpListener` surface is complete; `TcpStream` is v1 (handshake
//! only). Read / write / shutdown come in follow-up passes.

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use crate::kernel::{Addr, Domain, Fd, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::addr::sealed::Sealed;
use crate::shim::tokio::net::ToSocketAddrs;
use crate::sys;

/// Matches Linux's `SOMAXCONN` default — we don't enforce it, but we
/// pick a sensible value when the caller doesn't specify one via
/// `listen()`.
const DEFAULT_BACKLOG: usize = 1024;

pub struct TcpListener {
    fd: Fd,
}

impl TcpListener {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = Sealed::to_socket_addr(&addr)?;
        let fd = sys(|k| {
            let fd = k.bind(&Addr::Inet(addr), Type::Stream)?;
            k.listen(fd, DEFAULT_BACKLOG)?;
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
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        sys(|k| k.close(self.fd));
    }
}

fn domain_of(peer: &SocketAddr) -> Domain {
    match peer {
        SocketAddr::V4(_) => Domain::Inet,
        SocketAddr::V6(_) => Domain::Inet6,
    }
}
