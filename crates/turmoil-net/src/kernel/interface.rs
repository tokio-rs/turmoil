//! The two interfaces in the shared-link network model.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An interface identity local to a host. Addresses on an interface are
/// aliases, not separate interfaces. Indices are stable within the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum InterfaceId {
    Loopback = 1,
    Link = 2,
}

impl InterfaceId {
    /// The socket interface index; zero is reserved for the default interface.
    pub fn index(self) -> u32 {
        self as u32
    }
}

/// A multicast output-interface choice, resolved to an interface when sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InterfaceSelection {
    /// Use the route for the destination and bound source address.
    /// Selected by IPv4's unspecified address or IPv6's interface index zero.
    Default,
    /// Use the selected interface regardless of the default route.
    Specific(InterfaceId),
}

/// A host interface. Every host has `lo` and a shared-link interface `eth0`;
/// `Net::add_host` assigns its addresses to `eth0`.
#[derive(Debug, Clone)]
pub struct Interface {
    pub id: InterfaceId,
    pub name: &'static str,
    pub addresses: Vec<IpAddr>,
    pub mtu: u32,
}

#[derive(Debug)]
pub(super) struct Interfaces {
    loopback: Interface,
    link: Interface,
}

impl Interfaces {
    pub fn new(mtu: u32, loopback_mtu: u32) -> Self {
        Self {
            loopback: Interface {
                id: InterfaceId::Loopback,
                name: "lo",
                addresses: vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()],
                mtu: loopback_mtu,
            },
            link: Interface {
                id: InterfaceId::Link,
                name: "eth0",
                addresses: Vec::new(),
                mtu,
            },
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Interface> {
        [&self.loopback, &self.link].into_iter()
    }

    pub fn get(&self, id: InterfaceId) -> &Interface {
        match id {
            InterfaceId::Loopback => &self.loopback,
            InterfaceId::Link => &self.link,
        }
    }

    pub fn add_address(&mut self, addr: IpAddr) {
        let interface = if addr.is_loopback() {
            &mut self.loopback
        } else {
            &mut self.link
        };
        if !interface.addresses.contains(&addr) {
            interface.addresses.push(addr);
        }
    }

    pub fn by_address(&self, addr: IpAddr) -> Option<&Interface> {
        if addr.is_loopback() {
            return Some(self.get(InterfaceId::Loopback));
        }
        self.iter().find(|i| i.addresses.contains(&addr))
    }

    pub fn by_index(&self, index: u32) -> Option<&Interface> {
        self.iter().find(|i| i.id.index() == index)
    }

    pub fn source(&self, id: InterfaceId, dst: IpAddr) -> Option<IpAddr> {
        self.get(id)
            .addresses
            .iter()
            .copied()
            .find(|ip| ip.is_ipv4() == dst.is_ipv4())
    }

    /// The default multicast route is the shared link when it has an
    /// address in this family. Loopback-only fixtures route multicast to lo.
    /// Used for memberships and sends without a selected output interface
    /// or an IPv4 unicast source bound to a local interface.
    pub fn multicast_route(&self, dst: IpAddr) -> InterfaceId {
        if self.source(InterfaceId::Link, dst).is_some() {
            InterfaceId::Link
        } else {
            InterfaceId::Loopback
        }
    }
}
