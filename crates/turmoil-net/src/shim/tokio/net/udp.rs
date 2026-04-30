//! Drop-in replacement for [`tokio::net::UdpSocket`].

use std::future::poll_fn;
use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll, Waker};

use tokio::io::ReadBuf;

use crate::kernel::{Addr, Fd, SocketOption, SocketOptionKind, Type};
use crate::shim::tokio::net::ToSocketAddrs;
use crate::sys;

#[derive(Debug)]
pub struct UdpSocket {
    fd: Fd,
}

impl UdpSocket {
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let addr = addr.to_socket_addr()?;
        let fd = sys(|k| k.bind(&Addr::Inet(addr), Type::Dgram))?;
        Ok(Self { fd })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match sys(|k| k.local_addr(self.fd))? {
            Addr::Inet(sa) => Ok(sa),
            Addr::Unix(_) => panic!("UdpSocket is Addr::Inet"),
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match sys(|k| k.peer_addr(self.fd))? {
            Addr::Inet(sa) => Ok(sa),
            Addr::Unix(_) => panic!("UdpSocket is Addr::Inet"),
        }
    }

    pub async fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let addr = addr.to_socket_addr()?;
        poll_fn(|cx| sys(|k| k.poll_connect(self.fd, cx, &Addr::Inet(addr)))).await
    }

    pub async fn send_to<A: ToSocketAddrs>(&self, buf: &[u8], target: A) -> io::Result<usize> {
        let target = target.to_socket_addr()?;
        poll_fn(|cx| sys(|k| k.poll_send_to(self.fd, cx, buf, &Addr::Inet(target)))).await
    }

    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        poll_fn(|cx| sys(|k| k.poll_send(self.fd, cx, buf))).await
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        poll_fn(|cx| {
            let mut rb = ReadBuf::new(buf);
            match sys(|k| k.poll_recv_from(self.fd, cx, &mut rb)) {
                Poll::Ready(Ok(Addr::Inet(peer))) => Poll::Ready(Ok((rb.filled().len(), peer))),
                Poll::Ready(Ok(Addr::Unix(_))) => {
                    panic!("UdpSocket is Addr::Inet")
                }
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        poll_fn(|cx| sys(|k| k.poll_recv(self.fd, cx, buf))).await
    }

    pub async fn peek_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        poll_fn(|cx| {
            let mut rb = ReadBuf::new(buf);
            match sys(|k| k.poll_peek_from(self.fd, cx, &mut rb)) {
                Poll::Ready(Ok(Addr::Inet(peer))) => Poll::Ready(Ok((rb.filled().len(), peer))),
                Poll::Ready(Ok(Addr::Unix(_))) => panic!("UdpSocket is Addr::Inet"),
                Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    pub fn try_send(&self, buf: &[u8]) -> io::Result<usize> {
        unwrap_try(sys(|k| k.poll_send(self.fd, &mut noop_cx(), buf)))
    }

    pub fn try_send_to(&self, buf: &[u8], target: SocketAddr) -> io::Result<usize> {
        unwrap_try(sys(|k| {
            k.poll_send_to(self.fd, &mut noop_cx(), buf, &Addr::Inet(target))
        }))
    }

    pub fn try_recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        match sys(|k| k.poll_recv(self.fd, &mut noop_cx(), buf)) {
            Poll::Ready(r) => r,
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    pub fn try_recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut rb = ReadBuf::new(buf);
        match sys(|k| k.poll_recv_from(self.fd, &mut noop_cx(), &mut rb)) {
            Poll::Ready(Ok(Addr::Inet(peer))) => Ok((rb.filled().len(), peer)),
            Poll::Ready(Ok(Addr::Unix(_))) => panic!("UdpSocket is Addr::Inet"),
            Poll::Ready(Err(e)) => Err(e),
            Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        sys(|k| k.set_option(self.fd, SocketOption::Broadcast(on)))
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        match sys(|k| k.get_option(self.fd, SocketOptionKind::Broadcast))? {
            SocketOption::Broadcast(v) => Ok(v),
            _ => unreachable!(),
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

fn noop_cx() -> Context<'static> {
    Context::from_waker(Waker::noop())
}

fn unwrap_try(p: Poll<io::Result<usize>>) -> io::Result<usize> {
    match p {
        Poll::Ready(r) => r,
        Poll::Pending => Err(io::ErrorKind::WouldBlock.into()),
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        sys(|k| k.close(self.fd));
    }
}
