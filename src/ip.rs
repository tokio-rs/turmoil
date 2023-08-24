use std::{
    fmt,
    net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr},
    num::ParseIntError,
    str::FromStr,
};

/// An address withing a subnet.
pub(crate) struct ScopedIpAddr {
    pub(crate) addr: IpAddr,
    #[allow(dead_code)]
    pub(crate) subnet: IpSubnet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpSubnet {
    V4(Ipv4Subnet),
    V6(Ipv6Subnet),
}

impl IpSubnet {
    pub fn prefix(&self) -> IpAddr {
        match self {
            IpSubnet::V4(v4) => v4.prefix().into(),
            IpSubnet::V6(v6) => v6.prefix().into(),
        }
    }

    pub fn prefixlen(&self) -> usize {
        match self {
            IpSubnet::V4(v4) => v4.prefixlen(),
            IpSubnet::V6(v6) => v6.prefixlen(),
        }
    }

    pub fn contains(&self, addr: IpAddr) -> bool {
        match (self, addr) {
            (IpSubnet::V4(v4), IpAddr::V4(addr)) => v4.contains(addr),
            (IpSubnet::V6(v6), IpAddr::V6(addr)) => v6.contains(addr),
            _ => false,
        }
    }

    pub fn intersects(&self, subnet: IpSubnet) -> bool {
        match (self, subnet) {
            (IpSubnet::V4(v4), IpSubnet::V4(subnet)) => v4.intersects(subnet),
            (IpSubnet::V6(v6), IpSubnet::V6(subnet)) => v6.intersects(subnet),
            _ => false,
        }
    }
}

impl fmt::Display for IpSubnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpSubnet::V4(v4) => v4.fmt(f),
            IpSubnet::V6(v6) => v6.fmt(f),
        }
    }
}

/// An error type that models errors when parsing IpSubnets.
///
/// The syntax is `<addr>/<prefixlen>`
#[derive(Debug)]
pub enum IpSubnetParsingError {
    AddrParseError(AddrParseError),
    IntParseError(ParseIntError),
    InvalidSubnetSyntax,
}

/// An IP subnet which speaks Ipv4, defined by a subnet prefix of
/// a given length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ipv4Subnet {
    subnet: Ipv4Addr,
    mask: Ipv4Addr,
}

impl Ipv4Subnet {
    /// Creates a new subnet, using a network prefix with a given length.
    ///
    /// All non-zero bit beyond the prefix length will be
    /// truncated.
    ///
    /// # Panics
    ///
    /// This function panics should the prefix length exceed 31.
    pub fn new(subnet: Ipv4Addr, prefixlen: usize) -> Self {
        assert!(
            prefixlen < 32,
            "Ipv4 subnets cannot have network prefixes longer than 31 bits"
        );
        let mask = prefixlen_to_mask_v4(prefixlen);
        let subnet = truncate_netmask_v4(subnet, mask);
        Self { subnet, mask }
    }

    /// Returns the network address of the given subnet.
    pub fn prefix(&self) -> Ipv4Addr {
        self.subnet
    }

    /// Returns the prefix length of the given subnet
    pub fn prefixlen(&self) -> usize {
        mask_to_prefixlen_v4(self.mask)
    }

    /// Checks wether a given address is contained within the
    /// subnet.
    pub fn contains(&self, addr: Ipv4Addr) -> bool {
        let addr = u32::from(addr);
        let start = u32::from(self.subnet);
        let end = u32::from(self.subnet) | !u32::from(self.mask);

        start <= addr && addr <= end
    }

    /// Checks whether two subnets intersect.
    pub fn intersects(&self, subnet: Ipv4Subnet) -> bool {
        let start = u32::from(self.subnet);
        let end = u32::from(self.subnet) | !u32::from(self.mask);
        let addr_start = u32::from(subnet.subnet);
        let addr_end = u32::from(subnet.subnet) | !u32::from(subnet.mask);

        start <= addr_end && addr_start <= end
    }
}

impl FromStr for Ipv4Subnet {
    type Err = IpSubnetParsingError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((lhs, rhs)) = s.split_once('/') else {
            return Err(IpSubnetParsingError::InvalidSubnetSyntax)
        };

        let addr = match lhs.parse() {
            Ok(addr) => addr,
            Err(e) => return Err(IpSubnetParsingError::AddrParseError(e)),
        };

        let prefixlen = match rhs.parse() {
            Ok(prefixlen) => prefixlen,
            Err(e) => return Err(IpSubnetParsingError::IntParseError(e)),
        };

        Ok(Ipv4Subnet::new(addr, prefixlen))
    }
}

impl fmt::Display for Ipv4Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.subnet, self.prefixlen())
    }
}

impl Default for Ipv4Subnet {
    fn default() -> Self {
        Ipv4Subnet::new(Ipv4Addr::new(192, 168, 0, 0), 16)
    }
}

// Helper functions

fn truncate_netmask_v4(subnet: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(subnet) & u32::from(mask))
}

fn prefixlen_to_mask_v4(prefixlen: usize) -> Ipv4Addr {
    Ipv4Addr::from(!(u32::MAX >> prefixlen))
}

fn mask_to_prefixlen_v4(mask: Ipv4Addr) -> usize {
    u32::from(mask).leading_ones() as usize
}

/// An IP subnet which speaks Ipv6, defined by a subnet prefix of
/// a given length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ipv6Subnet {
    subnet: Ipv6Addr,
    mask: Ipv6Addr,
}

impl Ipv6Subnet {
    /// Creates a new subnet, using a network prefix with a given length.
    ///
    /// All non-zero bit beyond the prefix length will be
    /// truncated.
    ///
    /// # Panics
    ///
    /// This function panics should the prefix length exceed 127.
    pub fn new(subnet: Ipv6Addr, prefixlen: usize) -> Self {
        assert!(
            prefixlen < 128,
            "Ipv4 subnets cannot have network prefixes longer than 31 bits"
        );
        let mask = prefixlen_to_mask_v6(prefixlen);
        let subnet = truncate_netmask_v6(subnet, mask);
        Self { subnet, mask }
    }

    /// Returns the network address of the given subnet.
    pub fn prefix(&self) -> Ipv6Addr {
        self.subnet
    }

    /// Returns the prefix length of the given subnet
    pub fn prefixlen(&self) -> usize {
        mask_to_prefixlen_v6(self.mask)
    }

    /// Checks wether a given address is contained within the
    /// subnet.
    pub fn contains(&self, addr: Ipv6Addr) -> bool {
        let addr = u128::from(addr);
        let start = u128::from(self.subnet);
        let end = u128::from(self.subnet) | !u128::from(self.mask);

        start <= addr && addr <= end
    }

    /// Checks whether two subnets intersect.
    pub fn intersects(&self, subnet: Ipv6Subnet) -> bool {
        let start = u128::from(self.subnet);
        let end = u128::from(self.subnet) | !u128::from(self.mask);
        let addr_start = u128::from(subnet.subnet);
        let addr_end = u128::from(subnet.subnet) | !u128::from(subnet.mask);

        start <= addr_end && addr_start <= end
    }
}

impl FromStr for Ipv6Subnet {
    type Err = IpSubnetParsingError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((lhs, rhs)) = s.split_once('/') else {
            return Err(IpSubnetParsingError::InvalidSubnetSyntax)
        };

        let addr = match lhs.parse() {
            Ok(addr) => addr,
            Err(e) => return Err(IpSubnetParsingError::AddrParseError(e)),
        };

        let prefixlen = match rhs.parse() {
            Ok(prefixlen) => prefixlen,
            Err(e) => return Err(IpSubnetParsingError::IntParseError(e)),
        };

        Ok(Ipv6Subnet::new(addr, prefixlen))
    }
}

impl fmt::Display for Ipv6Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.subnet, self.prefixlen())
    }
}

impl Default for Ipv6Subnet {
    fn default() -> Self {
        Ipv6Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 64)
    }
}

// Helper functions

fn truncate_netmask_v6(subnet: Ipv6Addr, mask: Ipv6Addr) -> Ipv6Addr {
    Ipv6Addr::from(u128::from(subnet) & u128::from(mask))
}

fn prefixlen_to_mask_v6(prefixlen: usize) -> Ipv6Addr {
    Ipv6Addr::from(!(u128::MAX >> prefixlen))
}

fn mask_to_prefixlen_v6(mask: Ipv6Addr) -> usize {
    u128::from(mask).leading_ones() as usize
}

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
    pub(crate) fn next(&mut self) -> ScopedIpAddr {
        match self {
            Self::V4(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = (host >> 8) as u8;
                let b = (host & 0xFF) as u8;

                ScopedIpAddr {
                    addr: IpAddr::V4(Ipv4Addr::new(192, 168, a, b)),
                    subnet: IpSubnet::V4(Ipv4Subnet::default()),
                }
            }
            Self::V6(next) => {
                let host = *next;
                *next = next.wrapping_add(1);

                let a = ((host >> 48) & 0xffff) as u16;
                let b = ((host >> 32) & 0xffff) as u16;
                let c = ((host >> 16) & 0xffff) as u16;
                let d = (host & 0xffff) as u16;

                ScopedIpAddr {
                    addr: IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, a, b, c, d)),
                    subnet: IpSubnet::V6(Ipv6Subnet::default()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{lookup, Builder, IpVersion, Ipv4Subnet, Ipv6Subnet, Result};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn ip_subnet_v4() {
        let subnet = Ipv4Subnet::new(Ipv4Addr::new(192, 168, 0, 0), 16);
        assert_eq!(subnet.prefix(), Ipv4Addr::new(192, 168, 0, 0));

        assert!(subnet.contains(Ipv4Addr::new(192, 168, 2, 24)));
        assert!(subnet.contains(Ipv4Addr::new(192, 168, 0, 0)));
        assert!(subnet.contains(Ipv4Addr::new(192, 168, 255, 255)));

        assert!(!subnet.contains(Ipv4Addr::new(192, 169, 2, 24)));
        assert!(!subnet.contains(Ipv4Addr::new(0, 0, 0, 0)));
        assert!(!subnet.contains(Ipv4Addr::new(255, 255, 255, 255)));

        assert!(subnet.intersects(Ipv4Subnet::new(Ipv4Addr::new(192, 168, 2, 0), 10)));
        assert!(!subnet.intersects(Ipv4Subnet::new(Ipv4Addr::new(193, 168, 2, 0), 10)));
    }

    #[test]
    fn ip_subnet_v6() {
        let subnet = Ipv6Subnet::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 64);
        assert_eq!(subnet.prefix(), Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0));

        assert!(subnet.contains(Ipv6Addr::new(0xfe80, 0, 0, 0, 2, 0, 24, 0)));
        assert!(subnet.contains(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0)));
        assert!(subnet.contains(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0xffff, 0xffff, 0xffff, 0xffff
        )));

        assert!(!subnet.contains(Ipv6Addr::new(0xfe80, 0, 0, 3, 0, 0, 0, 0)));
        assert!(!subnet.contains(Ipv6Addr::UNSPECIFIED));
        assert!(!subnet.contains(Ipv6Addr::new(
            0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff
        )));

        assert!(subnet.intersects(Ipv6Subnet::new(
            Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0),
            52
        )));
        assert!(!subnet.intersects(Ipv6Subnet::new(
            Ipv6Addr::new(0xfe81, 0, 0, 0, 0, 0, 0, 0),
            40
        )));
    }

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
