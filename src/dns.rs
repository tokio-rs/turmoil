use indexmap::IndexMap;
#[cfg(feature = "regex")]
use regex::Regex;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, Ipv6Addr};

/// Each new registered host has an IP in a subnet 192.168.0.0/24
/// This is just a choice.
/// localhost always resolves to 127.0.0.1, there is no localhost implementation for IPv6.
pub struct Dns {
    next: u16,
    names: IndexMap<String, IpAddr>,
}

pub trait ToIpAddr {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr;
}

pub trait ToIpAddrs {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr>;
}

/// A simulated version of `tokio::net::ToSocketAddrs`.
pub trait ToSocketAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr;
}

impl Dns {
    pub(crate) fn new() -> Dns {
        let mut names: IndexMap<String, IpAddr> = IndexMap::new();
        names.insert(
            "localhost".to_string(),
            std::net::Ipv4Addr::LOCALHOST.into(),
        );

        Dns { next: 1, names }
    }

    pub(crate) fn lookup(&mut self, addr: impl ToIpAddr) -> IpAddr {
        addr.to_ip_addr(self)
    }

    pub(crate) fn lookup_many(&mut self, addrs: impl ToIpAddrs) -> Vec<IpAddr> {
        addrs.to_ip_addrs(self)
    }

    pub(crate) fn reverse(&self, addr: IpAddr) -> &str {
        self.names
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(name, _)| name)
            .expect("no hostname found for ip address")
    }
}

impl ToIpAddr for String {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        (&self[..]).to_ip_addr(dns)
    }
}

impl<'a> ToIpAddr for &'a str {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        *dns.names.entry(self.to_string()).or_insert_with(|| {
            let host = dns.next;
            dns.next += 1;

            let a = (host >> 8) as u8;
            let b = (host & 0xFF) as u8;

            std::net::Ipv4Addr::new(192, 168, a, b).into()
        })
    }
}

impl ToIpAddr for IpAddr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        *self
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
        // When "x.y.a.b" is passed as a hostname.
        if let Ok(ip) = self.0.parse::<Ipv4Addr>() {
            return (ip, self.1).into();
        }

        if let Ok(ip) = self.0.parse::<Ipv6Addr>() {
            return (ip, self.1).into();
        }

        match dns.names.get(self.0) {
            Some(ip) => (*ip, self.1).into(),
            None => panic!("no ip address found for a hostname"),
        }
    }
}

impl ToSocketAddrs for SocketAddr {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        *self
    }
}

impl ToSocketAddrs for (IpAddr, u16) {
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
    use crate::{dns::Dns, ToSocketAddrs};

    #[test]
    fn parse_str() {
        let mut dns = Dns::new();
        let generated_addr = dns.lookup("foo");

        let hostname_port = "foo:5000".to_socket_addr(&dns);
        let ipv4_port = "127.0.0.1:5000";
        let ipv6_port = "[::1]:5000";

        assert_eq!(hostname_port, format!("{}:5000", generated_addr).parse().unwrap());
        assert_eq!(ipv4_port.to_socket_addr(&dns), ipv4_port.parse().unwrap());
        assert_eq!(ipv6_port.to_socket_addr(&dns), ipv6_port.parse().unwrap());
    }
}
