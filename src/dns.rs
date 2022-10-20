use indexmap::IndexMap;
use std::net::{IpAddr, SocketAddr};

pub struct Dns {
    next: u16,
    names: IndexMap<String, IpAddr>,
}

pub trait ToIpAddr {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr;
}

pub trait ToSocketAddr {
    fn to_socket_addr(&self) -> SocketAddr;
}

impl Dns {
    pub(crate) fn new() -> Dns {
        Dns {
            next: 1,
            names: IndexMap::new(),
        }
    }

    pub(crate) fn lookup(&mut self, addr: impl ToIpAddr) -> IpAddr {
        addr.to_ip_addr(self)
    }

    pub(crate) fn _reverse(&self, addr: IpAddr) -> &str {
        self.names
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(name, _)| name)
            .expect("no hostname found for socket address")
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

            std::net::Ipv4Addr::new(127, 0, a, b).into()
        })
    }
}

impl ToIpAddr for IpAddr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        *self
    }
}

impl ToSocketAddr for SocketAddr {
    fn to_socket_addr(&self) -> SocketAddr {
        *self
    }
}

impl ToSocketAddr for (IpAddr, u16) {
    fn to_socket_addr(&self) -> SocketAddr {
        (*self).into()
    }
}
