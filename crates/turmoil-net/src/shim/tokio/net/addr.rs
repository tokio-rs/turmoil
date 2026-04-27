//! Minimal [`tokio::net::ToSocketAddrs`] stand-in.
//!
//! Synchronous, DNS-free resolution only: anything that already is
//! (or cheaply converts to) a [`SocketAddr`], plus string parsing.
//! Hostname lookup is a TODO.
//!
//! Sealed — downstream crates can pass these types but can't add new
//! impls. Keeps us free to swap in an async resolver later.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub trait ToSocketAddrs: sealed::Sealed {}

pub(crate) mod sealed {
    use std::io;
    use std::net::SocketAddr;

    pub trait Sealed {
        fn to_socket_addr(&self) -> io::Result<SocketAddr>;
    }
}

fn parse(s: &str) -> io::Result<SocketAddr> {
    s.parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

impl ToSocketAddrs for SocketAddr {}
impl sealed::Sealed for SocketAddr {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(*self)
    }
}

impl ToSocketAddrs for SocketAddrV4 {}
impl sealed::Sealed for SocketAddrV4 {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V4(*self))
    }
}

impl ToSocketAddrs for SocketAddrV6 {}
impl sealed::Sealed for SocketAddrV6 {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V6(*self))
    }
}

impl ToSocketAddrs for (IpAddr, u16) {}
impl sealed::Sealed for (IpAddr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::new(self.0, self.1))
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {}
impl sealed::Sealed for (Ipv4Addr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from((self.0, self.1)))
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {}
impl sealed::Sealed for (Ipv6Addr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from((self.0, self.1)))
    }
}

impl ToSocketAddrs for str {}
impl sealed::Sealed for str {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        parse(self)
    }
}

impl ToSocketAddrs for String {}
impl sealed::Sealed for String {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        parse(self)
    }
}

impl<T: ToSocketAddrs + ?Sized> ToSocketAddrs for &T {}
impl<T: ToSocketAddrs + ?Sized> sealed::Sealed for &T {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        (**self).to_socket_addr()
    }
}
