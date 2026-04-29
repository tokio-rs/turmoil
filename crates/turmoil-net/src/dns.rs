//! Hostname → IP resolution.
//!
//! A [`Dns`] hands out IPs from 192.168.0.0/16. The first time a
//! hostname is looked up it gets the next address in the subnet;
//! subsequent lookups return the same address. Literal IPs pass
//! through unchanged.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use indexmap::IndexMap;

#[derive(Debug)]
pub struct Dns {
    /// Next host byte pair in 192.168.0.0/16.
    next: u32,
    names: IndexMap<String, IpAddr>,
}

impl Dns {
    pub fn new() -> Self {
        Self {
            next: 1,
            names: IndexMap::new(),
        }
    }

    /// Allocate an IP for `name` if it doesn't already have one, and
    /// return the mapped address. Idempotent. `"localhost"` and IP
    /// literals short-circuit without touching the name table.
    pub fn resolve(&mut self, name: &str) -> IpAddr {
        if let Some(ip) = reserved(name) {
            return ip;
        }
        if let Ok(ip) = name.parse::<IpAddr>() {
            return ip;
        }
        let next = &mut self.next;
        *self.names.entry(name.to_string()).or_insert_with(|| {
            let host = *next;
            *next = next.wrapping_add(1);
            IpAddr::V4(Ipv4Addr::new(
                192,
                168,
                (host >> 8) as u8,
                (host & 0xff) as u8,
            ))
        })
    }

    /// Resolve `name` if already mapped; never allocate. `"localhost"`
    /// and IP literals pass through.
    pub fn lookup(&self, name: &str) -> Option<IpAddr> {
        if let Some(ip) = reserved(name) {
            return Some(ip);
        }
        if let Ok(ip) = name.parse::<IpAddr>() {
            return Some(ip);
        }
        self.names.get(name).copied()
    }

    pub fn reverse(&self, addr: IpAddr) -> Option<&str> {
        self.names
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(name, _)| name.as_str())
    }
}

fn reserved(name: &str) -> Option<IpAddr> {
    match name {
        "localhost" => Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        _ => None,
    }
}

/// A value that resolves to a single [`IpAddr`] against the current
/// [`Dns`]. Implemented for string-like types (hostname or literal)
/// and the IP types themselves.
pub trait ToIpAddr: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr;

    /// Non-allocating variant — returns `None` if a hostname isn't
    /// already registered. IP literals and the IP types themselves
    /// always return `Some`. Used by read-only consumers like
    /// `netstat` that have no business inventing addresses.
    #[doc(hidden)]
    fn try_to_ip_addr(&self, dns: &Dns) -> Option<IpAddr>;
}

/// A value that resolves to zero or more [`IpAddr`]s. Blanket impl
/// for every [`ToIpAddr`].
pub trait ToIpAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr>;
}

impl ToIpAddr for str {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        dns.resolve(self)
    }
    fn try_to_ip_addr(&self, dns: &Dns) -> Option<IpAddr> {
        dns.lookup(self)
    }
}

impl ToIpAddr for String {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        dns.resolve(self)
    }
    fn try_to_ip_addr(&self, dns: &Dns) -> Option<IpAddr> {
        dns.lookup(self)
    }
}

impl ToIpAddr for &str {
    fn to_ip_addr(&self, dns: &mut Dns) -> IpAddr {
        dns.resolve(self)
    }
    fn try_to_ip_addr(&self, dns: &Dns) -> Option<IpAddr> {
        dns.lookup(self)
    }
}

impl ToIpAddr for IpAddr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        *self
    }
    fn try_to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(*self)
    }
}

impl ToIpAddr for Ipv4Addr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        IpAddr::V4(*self)
    }
    fn try_to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(IpAddr::V4(*self))
    }
}

impl ToIpAddr for Ipv6Addr {
    fn to_ip_addr(&self, _: &mut Dns) -> IpAddr {
        IpAddr::V6(*self)
    }
    fn try_to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(IpAddr::V6(*self))
    }
}

impl<T: ToIpAddr + ?Sized> ToIpAddrs for T {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        vec![self.to_ip_addr(dns)]
    }
}

impl<T: ToIpAddr, const N: usize> ToIpAddrs for [T; N] {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        self.iter().map(|t| t.to_ip_addr(dns)).collect()
    }
}

impl<T: ToIpAddr> ToIpAddrs for [T] {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        self.iter().map(|t| t.to_ip_addr(dns)).collect()
    }
}

impl<T: ToIpAddr> ToIpAddrs for Vec<T> {
    fn to_ip_addrs(&self, dns: &mut Dns) -> Vec<IpAddr> {
        self.as_slice().to_ip_addrs(dns)
    }
}

mod sealed {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    pub trait Sealed {}

    impl Sealed for str {}
    impl Sealed for String {}
    impl Sealed for &str {}
    impl Sealed for IpAddr {}
    impl Sealed for Ipv4Addr {}
    impl Sealed for Ipv6Addr {}
    impl<T, const N: usize> Sealed for [T; N] {}
    impl<T> Sealed for [T] {}
    impl<T> Sealed for Vec<T> {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_is_idempotent() {
        let mut dns = Dns::new();
        assert_eq!(dns.resolve("alpha"), dns.resolve("alpha"));
    }

    #[test]
    fn allocates_from_subnet() {
        let mut dns = Dns::new();
        assert_eq!(dns.resolve("a"), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)));
        assert_eq!(dns.resolve("b"), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 2)));
    }

    #[test]
    fn literal_ip_passes_through() {
        let mut dns = Dns::new();
        assert_eq!(
            dns.resolve("10.0.0.1"),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
        );
        assert!(dns.names.is_empty());
    }

    #[test]
    fn lookup_never_allocates() {
        let mut dns = Dns::new();
        assert!(dns.lookup("nope").is_none());
        dns.resolve("yep");
        assert!(dns.lookup("yep").is_some());
    }

    #[test]
    fn reverse_returns_name() {
        let mut dns = Dns::new();
        let ip = dns.resolve("server");
        assert_eq!(dns.reverse(ip), Some("server"));
    }

    #[test]
    fn localhost_resolves_to_loopback() {
        let mut dns = Dns::new();
        assert_eq!(dns.resolve("localhost"), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(
            dns.lookup("localhost"),
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        // It must not consume a subnet slot.
        assert_eq!(dns.resolve("a"), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)));
    }
}
