//! Kernel-shaped socket layer.
//!
//! Models POSIX socket primitives (`socket`, `bind`, `listen`, `accept`,
//! `connect`, `send`, `recv`, `close`, `shutdown`, `setsockopt`) over a
//! deterministic host runtime. Protocol state machines for TCP, UDP, and
//! UDS live in the submodules below.

pub mod options;
pub mod socket;
pub mod table;
pub mod tcp;
pub mod udp;
pub mod uds;
