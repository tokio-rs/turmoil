//! Simulated socket layer for turmoil.
//!
//! This crate provides a kernel-shaped socket layer that models
//! POSIX-style sockets over a deterministic host stack, and a
//! tokio-shaped shim ([`shim::tokio::net`]) layered on top for drop-in
//! use in existing code.

use std::cell::RefCell;

pub(crate) mod kernel;
pub mod shim;

use crate::kernel::Kernel;
pub use crate::kernel::KernelConfig;

thread_local! {
    static CURRENT: RefCell<Option<Net>> = const { RefCell::new(None) };
}

/// Per-thread network state installed by [`Net::enter`].
///
/// Follows tokio's `Handle::enter` → `EnterGuard` pattern. A test
/// builds a [`Net`], calls [`Net::enter`] to install it on the current
/// thread, and runs its workload inside the returned guard's scope;
/// on drop the slot is cleared.
///
/// For now `Net` just holds one [`Kernel`] — the "current host". The
/// fabric (inter-host transport) will layer on top later, at which
/// point `Net` will own the fabric for the lifetime of the test and
/// swap the current kernel as each host executes.
#[derive(Debug)]
pub struct Net {
    current: Kernel,
}

impl Net {
    pub fn new() -> Self {
        Self::with_config(KernelConfig::default())
    }

    /// Build a `Net` with non-default kernel limits (backlog, buffer
    /// caps, MTU). Useful for tests that want to exercise backpressure
    /// without pushing through hundreds of KiB of traffic.
    pub fn with_config(cfg: KernelConfig) -> Self {
        Self {
            current: Kernel::with_config(cfg),
        }
    }

    /// Install `self` as the current thread's network for the lifetime
    /// of the returned guard. Panics if another `Net` is already
    /// installed on this thread.
    pub fn enter(self) -> EnterGuard {
        CURRENT.with(|c| {
            let mut slot = c.borrow_mut();
            assert!(slot.is_none(), "another Net is already installed");
            *slot = Some(self);
        });
        EnterGuard { _priv: () }
    }
}

impl Default for Net {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard returned by [`Net::enter`]. Clears the thread-local slot
/// on drop.
#[must_use = "a Net is only active while the guard is held"]
pub struct EnterGuard {
    _priv: (),
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        CURRENT.with(|c| *c.borrow_mut() = None);
    }
}

/// Run `f` against the current thread's kernel. Panics if no `Net` has
/// been entered.
pub(crate) fn sys<R>(f: impl FnOnce(&mut Kernel) -> R) -> R {
    CURRENT.with(|c| {
        let mut cell = c.borrow_mut();
        let net = cell
            .as_mut()
            .expect("no Net installed — call Net::enter() first");
        f(&mut net.current)
    })
}

/// Temporary hooks for integration tests - these should be removed.
pub fn step() -> usize {
    sys(|k| k.egress().len())
}
pub fn add_address(addr: std::net::IpAddr) {
    sys(|k| k.add_address(addr));
}
