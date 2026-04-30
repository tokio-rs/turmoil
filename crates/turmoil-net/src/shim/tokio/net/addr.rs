//! Minimal [`tokio::net::ToSocketAddrs`] stand-in.
//!
//! Synchronous resolution against the current [`Net`]'s DNS table.
//! String inputs (`"server:9000"` or `("server", 9000)`) resolve
//! hostnames registered with [`Net::add_host`]; anything that is
//! already (or cheaply converts to) a [`SocketAddr`] passes through.
//!
//! Sealed — downstream crates can pass these types but can't add new
//! impls.
//!
//! [`Net`]: crate::Net
//! [`Net::add_host`]: crate::Net::add_host

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub trait ToSocketAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_socket_addr(&self) -> io::Result<SocketAddr>;
}

fn resolve(host: &str, port: u16) -> io::Result<SocketAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    match crate::lookup_host(host) {
        Some(ip) => Ok(SocketAddr::new(ip, port)),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no ip address found for hostname: {host}"),
        )),
    }
}

fn parse(s: &str) -> io::Result<SocketAddr> {
    if let Ok(sa) = s.parse::<SocketAddr>() {
        return Ok(sa);
    }
    // Split "host:port" and defer to DNS.
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid socket address"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid port value"))?;
    resolve(host, port)
}

impl ToSocketAddrs for SocketAddr {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(*self)
    }
}

impl ToSocketAddrs for SocketAddrV4 {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V4(*self))
    }
}

impl ToSocketAddrs for SocketAddrV6 {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V6(*self))
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::new(self.0, self.1))
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from((self.0, self.1)))
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::from((self.0, self.1)))
    }
}

impl ToSocketAddrs for (&str, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        resolve(self.0, self.1)
    }
}

impl ToSocketAddrs for (String, u16) {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        resolve(&self.0, self.1)
    }
}

impl ToSocketAddrs for str {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        parse(self)
    }
}

impl ToSocketAddrs for String {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        parse(self)
    }
}

impl<T: ToSocketAddrs + ?Sized> ToSocketAddrs for &T {
    fn to_socket_addr(&self) -> io::Result<SocketAddr> {
        (**self).to_socket_addr()
    }
}

mod sealed {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

    pub trait Sealed {}

    impl Sealed for SocketAddr {}
    impl Sealed for SocketAddrV4 {}
    impl Sealed for SocketAddrV6 {}
    impl Sealed for (IpAddr, u16) {}
    impl Sealed for (Ipv4Addr, u16) {}
    impl Sealed for (Ipv6Addr, u16) {}
    impl Sealed for (&str, u16) {}
    impl Sealed for (String, u16) {}
    impl Sealed for str {}
    impl Sealed for String {}
    impl<T: Sealed + ?Sized> Sealed for &T {}
}
