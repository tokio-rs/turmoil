//! Drop-in replacement for [`tokio::net::TcpListener`].

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

use crate::kernel::{Addr, Fd, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::tcp::TcpStream;
use crate::shim::tokio::net::ToSocketAddrs;
use crate::sys;

#[derive(Debug)]
pub struct TcpListener {
    fd: Fd,
}

impl TcpListener {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = addr.to_socket_addr()?;
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
            Poll::Ready(Ok((fd, peer))) => Poll::Ready(Ok((TcpStream::from_fd(fd), peer))),
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
