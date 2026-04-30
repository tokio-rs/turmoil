//! Batteries-included test fixtures.
//!
//! Fixtures build and own their tokio runtime so tests don't have to
//! think about runtime flavor or `LocalSet` setup — just write
//! `#[test] fn ... { fixture::lo(async { ... }); }`.
//!
//! # Scheduling
//!
//! The runtime is built with [`start_paused(true)`], which is what
//! turns the otherwise-useless `sleep(tick)` into the "drain this
//! LocalSet to idle, then advance time" primitive these fixtures rely
//! on. `run_until(sleep(tick))` returns once every task on the set is
//! parked on something *other than* the sleep — i.e. waiting on a
//! packet the fabric hasn't delivered yet. We step the fabric between
//! drains to turn those parks into wakes. No busy yielding, no
//! scheduler-turn-per-hop overhead.
//!
//! [`start_paused(true)`]: tokio::runtime::Builder::start_paused
//!
//! - [`lo`] runs a single future against a loopback-only `Net`.
//! - [`ClientServer`] runs N servers plus one client across a
//!   multi-host `Net`. Each role runs on its own [`LocalSet`], drained
//!   in order. The run ends when the client future resolves — servers
//!   are aborted.
//!
//! [`LocalSet`]: tokio::task::LocalSet

use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;

use tokio::task::LocalSet;
use tokio::time::sleep;

use crate::{KernelConfig, Net};

const NO_ADDRS: [IpAddr; 0] = [];

/// Per-drain tick. The value is arbitrary — nothing in our protocol
/// waits on time. It just has to be non-zero so the paused runtime
/// treats it as a real sleep and drains the LocalSet before advancing.
pub(crate) const TICK: Duration = Duration::from_millis(1);

mod client_server;
mod scheduler;
pub use client_server::ClientServer;
pub(crate) use scheduler::Scheduler;

/// Run `fut` against a one-host `Net` with no public IPs — 127.0.0.1
/// / ::1 only.
pub fn lo<Fut>(fut: Fut) -> Fut::Output
where
    Fut: Future + 'static,
    Fut::Output: 'static,
{
    lo_with_config(KernelConfig::default(), fut)
}

pub fn lo_with_config<Fut>(cfg: KernelConfig, fut: Fut) -> Fut::Output
where
    Fut: Future + 'static,
    Fut::Output: 'static,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .expect("build current_thread runtime");

    let mut net = Net::with_config(cfg);
    net.add_host(NO_ADDRS);
    let guard = net.enter();

    let guard_ref = &guard;
    let result = rt.block_on(async {
        let set = LocalSet::new();
        let handle = set.spawn_local(fut);
        let mut scheduler = Scheduler::new();
        loop {
            set.run_until(sleep(TICK)).await;
            scheduler.tick(guard_ref, TICK);
            if handle.is_finished() {
                break handle.await.unwrap();
            }
        }
    });
    drop(guard);
    result
}
