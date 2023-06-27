use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The address layout of the underlying network.
///
/// The default value is an Ipv4 subnet with addresses
/// in the range `192.168.0.0/16`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpNetwork {
    /// An Ipv4 capable network, with a given subnet address range.
    V4(Ipv4Network),
    /// An Ipv6 capable network, with a given subnet address range.
    V6(Ipv6Network),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ipv4Network {
    prefix: Ipv4Addr,
    mask: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ipv6Network {
    prefix: Ipv6Addr,
    mask: Ipv6Addr,
}

impl IpNetwork {
    pub(crate) fn iter(&self) -> IpNetworkIter {
        match self {
            IpNetwork::V4(v4) => IpNetworkIter::V4(v4.clone(), 1),
            IpNetwork::V6(v6) => IpNetworkIter::V6(v6.clone(), 1),
        }
    }
}

impl Ipv4Network {
    /// A new instance of `Ipv4Network`.
    ///
    /// The provided prefix is truncated according to the
    /// prefixlen.
    ///
    /// # Panics
    ///
    /// This function panic if the prefixlen exceeds 32.
    pub fn new(prefix: Ipv4Addr, prefixlen: usize) -> Ipv4Network {
        assert!(
            prefixlen <= 32,
            "prefix lengths greater than 32 are not possible in Ipv4 networks"
        );
        let mask = Ipv4Addr::from(!(u32::MAX >> prefixlen));
        let prefix = Ipv4Addr::from(u32::from(prefix) & u32::from(mask));
        Ipv4Network { prefix, mask }
    }
}

impl Ipv6Network {
    /// A new instance of `Ipv6Network`.
    ///
    /// The provided prefix is truncated according to the
    /// prefixlen.
    ///
    /// # Panics
    ///
    /// This function panic if the prefixlen exceeds 128.
    pub fn new(prefix: Ipv6Addr, prefixlen: usize) -> Ipv6Network {
        assert!(
            prefixlen <= 128,
            "prefix lengths greater than 128 are not possible in Ipv6 networks"
        );
        let mask = Ipv6Addr::from(!(u128::MAX >> prefixlen));
        let prefix = Ipv6Addr::from(u128::from(prefix) & u128::from(mask));
        Ipv6Network { prefix, mask }
    }
}

impl Default for IpNetwork {
    fn default() -> Self {
        IpNetwork::V4(Ipv4Network::default())
    }
}

impl Default for Ipv4Network {
    fn default() -> Self {
        Ipv4Network::new(Ipv4Addr::new(192, 168, 0, 0), 16)
    }
}

impl Default for Ipv6Network {
    fn default() -> Self {
        Ipv6Network::new(
            Ipv6Addr::from(0xfe80_0000_0000_0000_0000_0000_0000_0000),
            64,
        )
    }
}

impl From<Ipv4Network> for IpNetwork {
    fn from(value: Ipv4Network) -> Self {
        IpNetwork::V4(value)
    }
}

impl From<Ipv6Network> for IpNetwork {
    fn from(value: Ipv6Network) -> Self {
        IpNetwork::V6(value)
    }
}

pub(crate) enum IpNetworkIter {
    V4(Ipv4Network, u32),
    V6(Ipv6Network, u128),
}

impl IpNetworkIter {
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
    use super::Ipv6Network;
    use crate::{lookup, Builder, Ipv4Network, Result};
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
        let mut sim = Builder::new().ip_network(Ipv6Network::default()).build();
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
            .ip_network(Ipv4Network::new(Ipv4Addr::new(10, 1, 3, 0), 24))
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
            .ip_network(Ipv6Network::new(
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
}
