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
    pub fn index(self) -> u32 {
        self as u32
    }
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
pub(super) struct Interfaces([Interface; 2]);

impl Interfaces {
    pub fn new(mtu: u32, loopback_mtu: u32) -> Self {
        Self([
            Interface {
                id: InterfaceId::Loopback,
                name: "lo",
                addresses: vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()],
                mtu: loopback_mtu,
            },
            Interface {
                id: InterfaceId::Link,
                name: "eth0",
                addresses: Vec::new(),
                mtu,
            },
        ])
    }

    pub fn all(&self) -> &[Interface] {
        &self.0
    }

    pub fn get(&self, id: InterfaceId) -> &Interface {
        &self.0[id.index() as usize - 1]
    }

    pub fn add_address(&mut self, addr: IpAddr) {
        let id = if addr.is_loopback() { 0 } else { 1 };
        if !self.0[id].addresses.contains(&addr) {
            self.0[id].addresses.push(addr);
        }
    }

    pub fn by_address(&self, addr: IpAddr) -> Option<InterfaceId> {
        if addr.is_loopback() {
            return Some(InterfaceId::Loopback);
        }
        self.0
            .iter()
            .find(|i| i.addresses.contains(&addr))
            .map(|i| i.id)
    }

    pub fn by_index(&self, index: u32) -> Option<InterfaceId> {
        self.0.iter().find(|i| i.id.index() == index).map(|i| i.id)
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
    /// Socket binding does not select this route.
    pub fn multicast_route(&self, dst: IpAddr) -> InterfaceId {
        if self.source(InterfaceId::Link, dst).is_some() {
            InterfaceId::Link
        } else {
            InterfaceId::Loopback
        }
    }
}
