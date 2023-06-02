use indexmap::IndexMap;
#[cfg(feature = "regex")]
use regex::Regex;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use crate::ip::IpVersionAddrIter;

/// Each new host has an IP in the subnet defined by the
/// ip version of the simulation.
///
/// Ipv4 simulations use the subnet 192.168.0.0/16.
/// Ipv6 simulations use the link local subnet fe80:::/64
pub struct Dns {
    addrs: IpVersionAddrIter,
    names: IndexMap<String, IpAddr>,
}

/// Converts or resolves to an [`IpAddr`].
pub trait ToIpAddr: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr;
}

/// Converts or resolves to one or more [`IpAddr`] values.
pub trait ToIpAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr>;
}

/// A simulated version of `tokio::net::ToSocketAddrs`.
pub trait ToSocketAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr;
}

impl Dns {
    pub(crate) fn new(addrs: IpVersionAddrIter) -> Dns {
        Dns {
            addrs,
            names: IndexMap::new(),
        }
    }

    pub(crate) fn lookup(&mut self, addr: impl ToIpAddr) -> IpAddr {
        addr.to_ip_addr(self)
    }

    pub(crate) fn lookup_many(&mut self, addrs: impl ToIpAddrs) -> Vec<IpAddr> {
        addrs.to_ip_addrs(self)
    }

    pub(crate) fn reverse(&self, addr: IpAddr) -> Option<&str> {
        self.names
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(name, _)| name.as_str())
    }
}

impl ToIpAddr for String {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        (&self[..]).to_ip_addr(dns)
    }
}

impl<'a> ToIpAddr for &'a str {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        if let Ok(ipaddr) = self.parse() {
            return ipaddr;
        }

        *dns.names
            .entry(self.to_string())
            .or_insert_with(|| dns.addrs.next())
    }
}

impl ToIpAddr for IpAddr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        *self
    }
}

impl ToIpAddr for Ipv4Addr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        IpAddr::V4(*self)
    }
}

impl ToIpAddr for Ipv6Addr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        IpAddr::V6(*self)
    }
}

impl<T> ToIpAddrs for T
where
    T: ToIpAddr,
{
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        vec![self.to_ip_addr(dns)]
    }
}

#[cfg(feature = "regex")]
impl ToIpAddrs for Regex {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        #[allow(clippy::needless_collect)]
        let hosts = dns.names.keys().cloned().collect::<Vec<_>>();
        hosts
            .into_iter()
            .filter_map(|h| self.is_match(&h).then(|| h.to_ip_addr(dns)))
            .collect::<Vec<_>>()
    }
}

// Hostname and port
impl ToSocketAddrs for (String, u16) {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        (&self.0[..], self.1).to_socket_addr(dns)
    }
}

impl<'a> ToSocketAddrs for (&'a str, u16) {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        // When IP address is passed directly as a str.
        if let Ok(ip) = self.0.parse::<IpAddr>() {
            return (ip, self.1).into();
        }

        match dns.names.get(self.0) {
            Some(ip) => (*ip, self.1).into(),
            None => panic!("no ip address found for a hostname: {}", self.0),
        }
    }
}

impl ToSocketAddrs for SocketAddr {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        *self
    }
}

impl ToSocketAddrs for SocketAddrV4 {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        SocketAddr::V4(*self)
    }
}

impl ToSocketAddrs for SocketAddrV6 {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        SocketAddr::V6(*self)
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        (*self).into()
    }
}

impl ToSocketAddrs for (Ipv4Addr, u16) {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        (*self).into()
    }
}

impl ToSocketAddrs for (Ipv6Addr, u16) {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        (*self).into()
    }
}

impl<T: ToSocketAddrs + ?Sized> ToSocketAddrs for &T {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        (**self).to_socket_addr(dns)
    }
}

impl ToSocketAddrs for str {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        let socketaddr: Result<SocketAddr, _> = self.parse();

        if let Ok(s) = socketaddr {
            return s;
        }

        // Borrowed from std
        // https://github.com/rust-lang/rust/blob/1b225414f325593f974c6b41e671a0a0dc5d7d5e/library/std/src/sys_common/net.rs#L175
        macro_rules! try_opt {
            ($e:expr, $msg:expr) => {
                match $e {
                    Some(r) => r,
                    None => panic!("Unable to parse dns: {}", $msg),
                }
            };
        }

        // split the string by ':' and convert the second part to u16
        let (host, port_str) = try_opt!(self.rsplit_once(':'), "invalid socket address");
        let port: u16 = try_opt!(port_str.parse().ok(), "invalid port value");

        (host, port).to_socket_addr(dns)
    }
}

impl ToSocketAddrs for String {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        self.as_str().to_socket_addr(dns)
    }
}

mod sealed {

    pub trait Sealed {}

    impl<T: ?Sized> Sealed for T {}
}

#[cfg(test)]
mod tests {
    use crate::{dns::Dns, ip::IpVersionAddrIter, ToSocketAddrs};
    use std::net::Ipv4Addr;

    #[test]
    fn parse_str() {
        let mut dns = Dns::new(IpVersionAddrIter::default());
        let generated_addr = dns.lookup("foo");

        let hostname_port = "foo:5000".to_socket_addr(&dns);
        let ipv4_port = "127.0.0.1:5000";
        let ipv6_port = "[::1]:5000";

        assert_eq!(
            hostname_port,
            format!("{generated_addr}:5000").parse().unwrap()
        );
        assert_eq!(ipv4_port.to_socket_addr(&dns), ipv4_port.parse().unwrap());
        assert_eq!(ipv6_port.to_socket_addr(&dns), ipv6_port.parse().unwrap());
    }

    #[test]
    fn raw_value_parsing() {
        // lookups of raw ip addrs should be consistent
        // between to_ip_addr() and to_socket_addr()
        // for &str and IpAddr
        let mut dns = Dns::new(IpVersionAddrIter::default());
        let addr = dns.lookup(Ipv4Addr::new(192, 168, 2, 2));
        assert_eq!(addr, Ipv4Addr::new(192, 168, 2, 2));

        let addr = dns.lookup("192.168.3.3");
        assert_eq!(addr, Ipv4Addr::new(192, 168, 3, 3));

        let addr = "192.168.3.3:0".to_socket_addr(&dns);
        assert_eq!(addr.ip(), Ipv4Addr::new(192, 168, 3, 3));
    }
}
