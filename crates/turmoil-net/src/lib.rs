//! Simulated socket layer for turmoil.
//!
//! This crate provides a kernel-shaped socket API ([`kernel`]) that models
//! POSIX-style sockets over a deterministic host runtime, and a tokio-shaped
//! shim ([`shim::tokio::net`]) layered on top for drop-in use in existing code.
//!
//! The kernel layer is the authoritative implementation: address families
//! (`AF_INET`, `AF_INET6`, `AF_UNIX`), socket types (`SOCK_STREAM`,
//! `SOCK_DGRAM`), and the TCP/UDP/UDS state machines live there. The shim
//! layer is a thin translation onto those primitives.
//!
//! # Integration with a host runtime
//!
//! `turmoil-net` is runtime-agnostic. A host runtime (e.g. the `turmoil`
//! crate) implements the [`NetRuntime`] trait to plug in its notion of the
//! current host, RNG, timer, and inter-host delivery. This keeps the socket
//! layer a standalone crate rather than coupling it to `turmoil`'s `World`.

pub mod kernel;
pub mod shim;

use std::net::IpAddr;

/// Host runtime hooks that `turmoil-net` needs to drive the socket layer.
///
/// Implemented by the embedding simulator (e.g. the `turmoil` crate) so that
/// `turmoil-net` can stay runtime-agnostic. The exact shape of this trait is
/// expected to evolve as the kernel layer lands — keep it minimal.
pub trait NetRuntime {
    /// IP address of the host currently executing.
    fn current_host(&self) -> IpAddr;

    /// Deliver a packet/segment to another host's inbox.
    ///
    /// The concrete envelope type is TBD — this signature is a placeholder
    /// while the kernel layer is being sketched out.
    fn deliver(&self, dst: IpAddr, bytes: bytes::Bytes);
}
