use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The kinds of networks that can be simulated in turmoil
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IpVersion {
    /// An Ipv4 network with an address space of 192.168.0.0/16
    #[default]
    V4,
    /// An local area Ipv6 network with an address space of fe80::/64
    V6,
}

impl IpVersion {
    pub(crate) fn iter(&self) -> IpVersionAddrIter {
        match self {
            Self::V4 => IpVersionAddrIter::V4(1),
            Self::V6 => IpVersionAddrIter::V6(1),
        }
    }
}

#[derive(Debug)]
pub(crate) enum IpVersionAddrIter {
    /// the next ip addr without the network prefix, as u32
    V4(u32),
    /// the next ip addr without the network prefix, as u128
    V6(u128),
}

impl Default for IpVersionAddrIter {
    fn default() -> Self {
        Self::V4(1)
    }
}

impl IpVersionAddrIter {
    pub(crate) fn next(&mut self) -> IpAddr {
        match self {
            Self::V4(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = (host >> 8) as u8;
                let b = (host & 0xFF) as u8;

                IpAddr::V4(Ipv4Addr::new(192, 168, a, b))
            }
            Self::V6(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = ((host >> 48) & 0xffff) as u16;
                let b = ((host >> 32) & 0xffff) as u16;
                let c = ((host >> 16) & 0xffff) as u16;
                let d = (host & 0xffff) as u16;

                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, a, b, c, d))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lookup, Builder, IpVersion, Result};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn ip_version_v4() -> Result {
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
    fn ip_version_v6() -> Result {
        let mut sim = Builder::new().ip_version(IpVersion::V6).build();
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
}
