//! Simulated socket layer for turmoil.
//!
//! This crate provides a kernel-shaped socket layer that models
//! POSIX-style sockets over a deterministic host stack, and a
//! tokio-shaped shim ([`shim::tokio::net`]) layered on top for drop-in
//! use in existing code.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::io::{Error, ErrorKind};
use std::net::IpAddr;

pub(crate) mod kernel;
pub mod shim;

use crate::kernel::Kernel;
pub use crate::kernel::KernelConfig;

thread_local! {
    static CURRENT: RefCell<Option<Net>> = const { RefCell::new(None) };
}

/// Opaque identifier for a host registered on a [`Net`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct HostId(u32);

/// A simulated host: one kernel plus the set of IP addresses it owns.
///
/// Loopback (`127.0.0.1`, `::1`) is implicit — it never appears in
/// `addrs` but `Kernel::is_local` treats it as local for free.
#[derive(Debug)]
pub(crate) struct Host {
    #[allow(dead_code)]
    pub(crate) addrs: Vec<IpAddr>,
    pub(crate) kernel: Kernel,
}

/// Per-thread network state installed by [`Net::enter`].
///
/// Follows tokio's `Handle::enter` → `EnterGuard` pattern. A test
/// builds a [`Net`], calls [`Net::enter`] to install it on the current
/// thread, and runs its workload inside the returned guard's scope;
/// on drop the slot is cleared.
///
/// `Net` owns every registered host plus an IpAddr→HostId index for
/// inter-host routing. `current` tracks which host `sys()` talks to —
/// mutated by the scheduler as it runs each host's tasks, or pinned
/// to a single host for the simple "one host" test flows.
#[derive(Debug)]
pub struct Net {
    hosts: HashMap<HostId, Host>,
    ip_to_host: HashMap<IpAddr, HostId>,
    current: Option<HostId>,
    next_id: u32,
    default_cfg: KernelConfig,
}

impl Net {
    /// Empty `Net` with default kernel config. No hosts, no current —
    /// register hosts via [`Net::add_host`] before entering it.
    pub fn new() -> Self {
        Self::with_config(KernelConfig::default())
    }

    /// Empty `Net` with a non-default kernel config applied to every
    /// host added later.
    pub fn with_config(cfg: KernelConfig) -> Self {
        Self {
            hosts: HashMap::new(),
            ip_to_host: HashMap::new(),
            current: None,
            next_id: 0,
            default_cfg: cfg,
        }
    }

    /// Register a new host with the given public IP addresses.
    ///
    /// Validates: at most one address per family, no loopback
    /// (loopback is always implicit), no duplicates within the set or
    /// across other hosts on this `Net`.
    ///
    /// If no host is currently selected, the newly added host becomes
    /// current. Returns `&mut Self` for builder-style chaining. Panics
    /// on validation failure — this is a setup API, not a runtime one.
    pub fn add_host<I>(&mut self, addrs: I) -> &mut Self
    where
        I: IntoIterator<Item = IpAddr>,
    {
        let addrs: Vec<_> = addrs.into_iter().collect();
        let id = self.alloc_host(addrs).expect("add_host validation failed");
        if self.current.is_none() {
            self.current = Some(id);
        }
        self
    }

    fn alloc_host(&mut self, addrs: Vec<IpAddr>) -> std::io::Result<HostId> {
        let mut v4_seen = false;
        let mut v6_seen = false;
        for a in &addrs {
            if a.is_loopback() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "loopback is implicit; do not register it explicitly",
                ));
            }
            if self.ip_to_host.contains_key(a) {
                return Err(Error::from(ErrorKind::AddrInUse));
            }
            match a {
                IpAddr::V4(_) if v4_seen => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "at most one IPv4 address per host",
                    ));
                }
                IpAddr::V6(_) if v6_seen => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "at most one IPv6 address per host",
                    ));
                }
                IpAddr::V4(_) => v4_seen = true,
                IpAddr::V6(_) => v6_seen = true,
            }
        }

        let id = HostId(self.next_id);
        self.next_id += 1;

        let mut kernel = Kernel::with_config(self.default_cfg.clone());
        for a in &addrs {
            kernel.add_address(*a);
        }
        for a in &addrs {
            self.ip_to_host.insert(*a, id);
        }
        self.hosts.insert(id, Host { addrs, kernel });
        Ok(id)
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

    /// Build a loopback-only `Net` (one host, no public IPs) and run
    /// `fut` inside it. Auto-steps the kernel between scheduler turns
    /// so loopback sends/receives flow without manual `step()` calls.
    pub async fn lo<Fut>(fut: Fut) -> Fut::Output
    where
        Fut: Future,
    {
        Self::lo_with_config(KernelConfig::default(), fut).await
    }

    /// [`Net::lo`] with a custom [`KernelConfig`] applied to the
    /// loopback host.
    pub async fn lo_with_config<Fut>(cfg: KernelConfig, fut: Fut) -> Fut::Output
    where
        Fut: Future,
    {
        let mut net = Net::with_config(cfg);
        net.add_host(std::iter::empty::<IpAddr>());
        net.run(fut).await
    }

    /// Install `self` on the current thread, run `fut` to completion,
    /// and tear down. Interleaves an always-on background stepper
    /// that drives each host's kernel egress whenever the scheduler
    /// gives it a turn — no manual stepping required.
    pub async fn run<Fut>(self, fut: Fut) -> Fut::Output
    where
        Fut: Future,
    {
        let _guard = self.enter();
        // Background stepper: spins on `yield_now` and drives the
        // current host's egress whenever the scheduler hands it time.
        // On a single-thread tokio runtime this means every task yield
        // gets a step before it's polled again.
        let stepper = tokio::spawn(async {
            loop {
                sys(|k| {
                    let _ = k.egress();
                });
                tokio::task::yield_now().await;
            }
        });
        let result = fut.await;
        stepper.abort();
        result
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

/// Run `f` against the current host's kernel. Panics if no `Net` has
/// been entered, or if the entered `Net` has no current host.
pub(crate) fn sys<R>(f: impl FnOnce(&mut Kernel) -> R) -> R {
    CURRENT.with(|c| {
        let mut cell = c.borrow_mut();
        let net = cell
            .as_mut()
            .expect("no Net installed — call Net::enter() first");
        let id = net
            .current
            .expect("no current host — register one with Net::add_host()");
        let host = net
            .hosts
            .get_mut(&id)
            .expect("current HostId points at a live host");
        f(&mut host.kernel)
    })
}
