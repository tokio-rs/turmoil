use indexmap::IndexMap;
#[cfg(feature = "regex")]
use regex::Regex;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

    /// Resolve a registered name without allocating an address.
    pub(crate) fn get(&self, name: &str) -> Option<IpAddr> {
        self.names.get(name).copied()
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

impl ToIpAddr for &str {
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

mod sealed {

    pub trait Sealed {}

    impl<T: ?Sized> Sealed for T {}
}

#[cfg(test)]
mod tests {
    use crate::{dns::Dns, ip::IpVersionAddrIter};
    use std::net::Ipv4Addr;

    #[test]
    fn get_does_not_allocate() {
        let mut dns = Dns::new(IpVersionAddrIter::default());
        assert_eq!(dns.get("foo"), None);
        let addr = dns.lookup("foo");
        assert_eq!(dns.get("foo"), Some(addr));
    }

    #[test]
    fn raw_value_parsing() {
        // Literal IPs pass through without registering a name.
        let mut dns = Dns::new(IpVersionAddrIter::default());
        let addr = dns.lookup(Ipv4Addr::new(192, 168, 2, 2));
        assert_eq!(addr, Ipv4Addr::new(192, 168, 2, 2));

        let addr = dns.lookup("192.168.3.3");
        assert_eq!(addr, Ipv4Addr::new(192, 168, 3, 3));
        assert_eq!(dns.get("192.168.3.3"), None);
    }
}
