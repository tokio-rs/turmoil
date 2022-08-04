use indexmap::IndexMap;
use std::cell::RefCell;
use std::net::SocketAddr;

pub struct Dns {
    inner: RefCell<Inner>,
}

struct Inner {
    next: u16,
    names: IndexMap<String, u16>,
}

pub trait ToSocketAddr {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr;
}

impl Dns {
    pub(crate) fn new() -> Dns {
        Dns {
            inner: RefCell::new(Inner {
                next: 1,
                names: IndexMap::new(),
            }),
        }
    }

    pub(crate) fn lookup(&self, addr: impl ToSocketAddr) -> SocketAddr {
        addr.to_socket_addr(self)
    }
}

impl ToSocketAddr for String {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        (&self[..]).to_socket_addr(dns)
    }
}

impl<'a> ToSocketAddr for &'a str {
    fn to_socket_addr(&self, dns: &Dns) -> SocketAddr {
        let mut inner = dns.inner.borrow_mut();
        let inner = &mut *inner;

        let host = inner.names.entry(self.to_string()).or_insert_with(|| {
            let next = inner.next;
            inner.next += 1;
            next
        });

        let a = (*host >> 8) as u8;
        let b = (*host & 0xFF) as u8;

        (std::net::Ipv4Addr::new(127, 0, a, b), 3000).into()
    }
}

impl ToSocketAddr for SocketAddr {
    fn to_socket_addr(&self, _: &Dns) -> SocketAddr {
        *self
    }
}
