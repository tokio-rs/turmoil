//! Batteries-included test fixtures.
//!
//! - [`lo`] runs a single future against a loopback-only `Net` with
//!   a background stepper — the shape most tests in this crate use.
//! - [`ClientServer`] runs N servers plus one client against a
//!   multi-host `Net`. Each role runs as its own host on its own
//!   [`tokio::task::LocalSet`], interleaved with fabric steps. The
//!   run ends when the client's future resolves — servers are
//!   aborted.

use std::future::Future;
use std::net::IpAddr;

use crate::{KernelConfig, Net};

mod client_server;
pub use client_server::ClientServer;

/// Run `fut` against a one-host `Net` with no public IPs — 127.0.0.1
/// / ::1 only.
pub async fn lo<Fut>(fut: Fut) -> Fut::Output
where
    Fut: Future,
{
    lo_with_config(KernelConfig::default(), fut).await
}

pub async fn lo_with_config<Fut>(cfg: KernelConfig, fut: Fut) -> Fut::Output
where
    Fut: Future,
{
    let mut net = Net::with_config(cfg);
    net.add_host(std::iter::empty::<IpAddr>());
    let guard = net.enter();

    // Background stepper drives the fabric on every scheduler turn.
    let stepper = tokio::spawn(async {
        loop {
            crate::CURRENT.with(|c| {
                c.borrow_mut()
                    .as_mut()
                    .expect("guard is live")
                    .fabric
                    .step();
            });
            tokio::task::yield_now().await;
        }
    });

    let result = fut.await;
    stepper.abort();
    drop(guard);
    result
}
