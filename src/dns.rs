use indexmap::IndexMap;
use std::net::SocketAddr;

pub struct Dns {
    next: u16,
    names: IndexMap<String, u16>,
}

pub trait ToSocketAddr {
    fn to_socket_addr(&self, dns: &mut Dns) -> SocketAddr;
}

impl Dns {
    pub(crate) fn new() -> Dns {
        Dns {
            next: 1,
            names: IndexMap::new(),
        }
    }

    pub(crate) fn lookup(&mut self, addr: impl ToSocketAddr) -> SocketAddr {
        addr.to_socket_addr(self)
    }
}

impl ToSocketAddr for String {
    fn to_socket_addr(&self, dns: &mut Dns) -> SocketAddr {
        (&self[..]).to_socket_addr(dns)
    }
}

impl<'a> ToSocketAddr for &'a str {
    fn to_socket_addr(&self, dns: &mut Dns) -> SocketAddr {
        let host = dns.names.entry(self.to_string()).or_insert_with(|| {
            let next = dns.next;
            dns.next += 1;
            next
        });

        let a = (*host >> 8) as u8;
        let b = (*host & 0xFF) as u8;

        (std::net::Ipv4Addr::new(127, 0, a, b), 3000).into()
    }
}

impl ToSocketAddr for SocketAddr {
    fn to_socket_addr(&self, _: &mut Dns) -> SocketAddr {
        *self
    }
}
