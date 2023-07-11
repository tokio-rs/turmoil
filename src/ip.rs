use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A subnet defining the available addresses in the
/// underlying network.
///
/// The default value is an Ipv4 subnet with addresses
/// in the range `192.168.0.0/16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpSubnet {
    /// An Ipv4 capable subnet, with a given address range.
    V4(Ipv4Subnet),
    /// An Ipv6 capable subnet, with a given address range.
    V6(Ipv6Subnet),
}

/// An Ipv4 subnet, defining a range of availale Ipv4 addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Subnet {
    prefix: Ipv4Addr,
    mask: Ipv4Addr,
}

/// An Ipv6 subnet, defining a range of availale Ipv6 addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv6Subnet {
    prefix: Ipv6Addr,
    mask: Ipv6Addr,
}

impl IpSubnet {
    pub(crate) fn iter(&self) -> IpSubnetIter {
        match self {
            IpSubnet::V4(v4) => IpSubnetIter::V4(*v4, 1),
            IpSubnet::V6(v6) => IpSubnetIter::V6(*v6, 1),
        }
    }

    pub fn contains(&self, addr: IpAddr) -> bool {
        match (self, addr) {
            (Self::V4(net), IpAddr::V4(addr)) => net.contains(addr),
            (Self::V6(net), IpAddr::V6(addr)) => net.contains(addr),
            _ => false,
        }
    }
}

impl Ipv4Subnet {
    /// A new instance of `Ipv4Network`.
    ///
    /// The provided prefix is truncated according to the
    /// prefixlen.
    ///
    /// # Panics
    ///
    /// This function panic if the prefixlen exceeds 32.
    pub fn new(prefix: Ipv4Addr, prefixlen: usize) -> Ipv4Subnet {
        assert!(
            prefixlen <= 32,
            "prefix lengths greater than 32 are not possible in Ipv4 networks"
        );
        let mask = Ipv4Addr::from(!(u32::MAX >> prefixlen));
        let prefix = Ipv4Addr::from(u32::from(prefix) & u32::from(mask));
        Ipv4Subnet { prefix, mask }
    }

    pub fn contains(&self, addr: Ipv4Addr) -> bool {
        u32::from(self.prefix) == u32::from(addr) & u32::from(self.mask)
    }
}

impl Ipv6Subnet {
    /// A new instance of `Ipv6Network`.
    ///
    /// The provided prefix is truncated according to the
    /// prefixlen.
    ///
    /// # Panics
    ///
    /// This function panic if the prefixlen exceeds 128.
    pub fn new(prefix: Ipv6Addr, prefixlen: usize) -> Ipv6Subnet {
        assert!(
            prefixlen <= 128,
            "prefix lengths greater than 128 are not possible in Ipv6 networks"
        );
        let mask = Ipv6Addr::from(!(u128::MAX >> prefixlen));
        let prefix = Ipv6Addr::from(u128::from(prefix) & u128::from(mask));
        Ipv6Subnet { prefix, mask }
    }

    pub fn contains(&self, addr: Ipv6Addr) -> bool {
        u128::from(self.prefix) == u128::from(addr) & u128::from(self.mask)
    }
}

impl Default for IpSubnet {
    fn default() -> Self {
        IpSubnet::V4(Ipv4Subnet::default())
    }
}

impl Default for Ipv4Subnet {
    fn default() -> Self {
        Ipv4Subnet::new(Ipv4Addr::new(192, 168, 0, 0), 16)
    }
}

impl Default for Ipv6Subnet {
    fn default() -> Self {
        Ipv6Subnet::new(
            Ipv6Addr::from(0xfe80_0000_0000_0000_0000_0000_0000_0000),
            64,
        )
    }
}

impl From<Ipv4Subnet> for IpSubnet {
    fn from(value: Ipv4Subnet) -> Self {
        IpSubnet::V4(value)
    }
}

impl From<Ipv6Subnet> for IpSubnet {
    fn from(value: Ipv6Subnet) -> Self {
        IpSubnet::V6(value)
    }
}

pub(crate) enum IpSubnetIter {
    V4(Ipv4Subnet, u32),
    V6(Ipv6Subnet, u128),
}

impl IpSubnetIter {
    pub(crate) fn next(&mut self) -> IpAddr {
        match self {
            Self::V4(net, next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let host_masked = host & !u32::from(net.mask);
                IpAddr::V4(Ipv4Addr::from(u32::from(net.prefix) | host_masked))
            }
            Self::V6(net, next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let host_masked = host & !u128::from(net.mask);
                IpAddr::V6(Ipv6Addr::from(u128::from(net.prefix) | host_masked))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ipv6Subnet;
    use crate::{lookup, Builder, Ipv4Subnet, Result};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn default_ipv4() -> Result {
        let mut sim = Builder::new().build();
        sim.client("client", async move {
            assert_eq!(lookup("client"), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)));
            assert_eq!(lookup("server"), IpAddr::V4(Ipv4Addr::new(192, 168, 0, 2)));
            Ok(())
        });
        sim.client("server", async move { Ok(()) });

        assert_eq!(
            sim.lookup("client"),
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))
        );
        assert_eq!(
            sim.lookup("server"),
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 2))
        );

        sim.run()
    }

    #[test]
    fn default_ipv6() -> Result {
        let mut sim = Builder::new().ip_network(Ipv6Subnet::default()).build();
        sim.client("client", async move {
            assert_eq!(
                lookup("client"),
                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))
            );
            assert_eq!(
                lookup("server"),
                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 2))
            );
            Ok(())
        });
        sim.client("server", async move { Ok(()) });

        assert_eq!(
            sim.lookup("client"),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1))
        );
        assert_eq!(
            sim.lookup("server"),
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 2))
        );

        sim.run()
    }

    #[test]
    fn custom_ipv4() -> Result {
        let mut sim = Builder::new()
            .ip_network(Ipv4Subnet::new(Ipv4Addr::new(10, 1, 3, 0), 24))
            .build();

        sim.client("a", async move {
            assert_eq!(lookup("a"), Ipv4Addr::new(10, 1, 3, 1));
            Ok(())
        });
        sim.client("b", async move {
            assert_eq!(lookup("b"), Ipv4Addr::new(10, 1, 3, 2));
            Ok(())
        });
        sim.client("c", async move {
            assert_eq!(lookup("c"), Ipv4Addr::new(10, 1, 3, 3));
            Ok(())
        });

        sim.run()
    }

    #[test]
    fn custom_ipv6() -> Result {
        let mut sim = Builder::new()
            .ip_network(Ipv6Subnet::new(
                Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0),
                64,
            ))
            .build();

        sim.client("a", async move {
            assert_eq!(lookup("a"), Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 1));
            Ok(())
        });
        sim.client("b", async move {
            assert_eq!(lookup("b"), Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 2));
            Ok(())
        });
        sim.client("c", async move {
            assert_eq!(lookup("c"), Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 3));
            Ok(())
        });

        sim.run()
    }

    #[test]
    #[should_panic = "node address is not contained within the available subnet"]
    fn subnet_denies_invalid_addr_v4() {
        let mut sim = Builder::new()
            .ip_network(Ipv4Subnet::new(Ipv4Addr::new(1, 2, 3, 4), 16))
            .build();

        sim.client("30.0.0.0", async move { Ok(()) });
        unreachable!()
    }

    #[test]
    #[should_panic = "node address is not contained within the available subnet"]
    fn subnet_denies_invalid_addr_v6() {
        let mut sim = Builder::new()
            .ip_network(Ipv6Subnet::new(
                Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0),
                64,
            ))
            .build();

        sim.client("fc00:0001::bc", async move { Ok(()) });
        unreachable!()
    }
}
