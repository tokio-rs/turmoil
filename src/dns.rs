use indexmap::IndexMap;
#[cfg(feature = "regex")]
use regex::Regex;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use crate::{
    ip::{IpVersionAddrIter, ScopedIpAddr},
    IpSubnet, Ipv4Subnet, Ipv6Subnet,
};

/// Each new host has an IP in the subnet defined by the
/// ip version of the simulation.
///
/// Ipv4 simulations use the subnet 192.168.0.0/16.
/// Ipv6 simulations use the link local subnet fe80:::/64
pub struct Dns {
    addrs: IpVersionAddrIter,
    mapping: IndexMap<String, NodeInfo>,
}

pub struct NodeInfo {
    addrs: Vec<ScopedIpAddr>,
    // id: NodeIdentifer
}

/// Converts or resolves to an [`IpAddr`].
pub trait ToIpAddr: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addr(&self, dns: &Dns) -> Option<IpAddr>;

    #[doc(hidden)]
    fn to_name(&self, dns: &Dns) -> String;
}

/// Converts or resolves to one or more [`IpAddr`] values.
pub trait ToIpAddrs: sealed::Sealed {
    #[doc(hidden)]
    fn to_ip_addrs(&self, dns: &Dns) -> Vec<IpAddr>;
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
            mapping: IndexMap::new(),
        }
    }

    pub(crate) fn register(&mut self, addr: impl ToIpAddr) -> IpAddr {
        // Manual lookup
        let scoped_addr = addr
            .to_ip_addr(self)
            .map(|addr| {
                if addr.is_ipv4() {
                    ScopedIpAddr {
                        addr,
                        subnet: IpSubnet::V4(Ipv4Subnet::default()),
                    }
                } else {
                    ScopedIpAddr {
                        addr,
                        subnet: IpSubnet::V6(Ipv6Subnet::default()),
                    }
                }
            })
            .unwrap_or_else(|| self.addrs.next());

        let name = self.reverse(addr);
        let addr = scoped_addr.addr;

        self.mapping.insert(
            name,
            NodeInfo {
                addrs: vec![scoped_addr],
            },
        );
        addr
    }

    pub(crate) fn lookup(&mut self, addr: impl ToIpAddr) -> IpAddr {
        addr.to_ip_addr(self).expect("failed to lookup ip addr")
    }

    pub(crate) fn lookup_many(&mut self, addrs: impl ToIpAddrs) -> Vec<IpAddr> {
        addrs.to_ip_addrs(self)
    }

    pub(crate) fn reverse(&self, addr: impl ToIpAddr) -> String {
        addr.to_name(self)
    }
}

impl ToIpAddr for String {
    fn to_ip_addr(&self, dns: &Dns) -> Option<IpAddr> {
        (&self[..]).to_ip_addr(dns)
    }

    fn to_name(&self, dns: &Dns) -> String {
        (&self[..]).to_name(dns)
    }
}

impl<'a> ToIpAddr for &'a str {
    fn to_ip_addr(&self, dns: &Dns) -> Option<IpAddr> {
        if let Ok(ipaddr) = self.parse() {
            return Some(ipaddr);
        }

        let info = dns.mapping.get(*self)?;

        // Quick hack, as long as multiple ips are not yet implemented
        Some(info.addrs[0].addr)
    }

    fn to_name(&self, _: &Dns) -> String {
        self.to_string()
    }
}

impl ToIpAddr for IpAddr {
    fn to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(*self)
    }

    fn to_name(&self, dns: &Dns) -> String {
        if let Some((name, _)) = dns
            .mapping
            .iter()
            .find(|(_, info)| info.addrs.iter().any(|scoped| scoped.addr == *self))
        {
            return name.to_string();
        }

        self.to_string()
    }
}

impl ToIpAddr for Ipv4Addr {
    fn to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(IpAddr::V4(*self))
    }
    fn to_name(&self, dns: &Dns) -> String {
        IpAddr::V4(*self).to_name(dns)
    }
}

impl ToIpAddr for Ipv6Addr {
    fn to_ip_addr(&self, _: &Dns) -> Option<IpAddr> {
        Some(IpAddr::V6(*self))
    }
    fn to_name(&self, dns: &Dns) -> String {
        IpAddr::V6(*self).to_name(dns)
    }
}

impl<T> ToIpAddrs for T
where
    T: ToIpAddr,
{
    fn to_ip_addrs(&self, dns: &Dns) -> Vec<IpAddr> {
        if let Some(ipaddr) = self.to_ip_addr(dns) {
            vec![ipaddr]
        } else {
            Vec::new()
        }
    }
}

#[cfg(feature = "regex")]
impl ToIpAddrs for Regex {
    fn to_ip_addrs(&self, dns: &Dns) -> Vec<IpAddr> {
        #[allow(clippy::needless_collect)]
        let hosts = dns.mapping.keys().cloned().collect::<Vec<_>>();
        hosts
            .into_iter()
            .filter_map(|h| self.is_match(&h).then(|| h.to_ip_addr(dns)))
            .flatten()
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

        match dns.mapping.get(self.0) {
            Some(info) => (info.addrs[0].addr, self.1).into(),
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
        let generated_addr = dns.register("foo");

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
