//! Multi-host client/server fixture.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::task::LocalSet;
use tokio::time::sleep;

use crate::fixture::{Scheduler, TICK};
use crate::{HostId, Net, ToIpAddrs};

type BoxFut = Pin<Box<dyn Future<Output = ()>>>;

/// Multi-host fixture with N servers plus one client. All host
/// futures run on a single [`LocalSet`] driven by a single paused
/// tokio runtime.
pub struct ClientServer {
    net: Net,
    servers: Vec<(HostId, BoxFut)>,
}

impl ClientServer {
    pub fn new() -> Self {
        Self {
            net: Net::new(),
            servers: Vec::new(),
        }
    }

    /// Register a server. `addrs` accepts hostnames or literal IPs;
    /// loopback is implicit. `fut` runs inside that host's scope —
    /// every `sys()` call from its socket operations sees this host
    /// as `current`.
    pub fn server<A, F>(mut self, addrs: A, fut: F) -> Self
    where
        A: ToIpAddrs,
        F: Future<Output = ()> + 'static,
    {
        let id = self.net.add_host(addrs);
        self.servers.push((id, Box::pin(fut)));
        self
    }

    /// Run the fixture with `fut` as the client. Every server is
    /// driven in parallel; the fixture returns `fut`'s output as
    /// soon as it resolves.
    pub fn run<A, T, F>(self, addrs: A, fut: F) -> T
    where
        A: ToIpAddrs,
        F: Future<Output = T> + 'static,
        T: 'static,
    {
        let Self { mut net, servers } = self;
        assert!(
            !servers.is_empty(),
            "ClientServer needs at least one server — use fixture::lo for single-host tests"
        );
        let client_id = net.add_host(addrs);
        let guard = net.enter();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("build current_thread runtime");

        let guard_ref = &guard;
        let result = rt.block_on(async move {
            let set = LocalSet::new();
            for (id, fut) in servers {
                set.spawn_local(HostScoped { id, inner: fut });
            }
            let client_handle = set.spawn_local(HostScoped {
                id: client_id,
                inner: Box::pin(fut),
            });

            let mut scheduler = Scheduler::new();
            loop {
                // Single LocalSet drain per iter → tokio clock += TICK.
                set.run_until(sleep(TICK)).await;
                // Scheduler drains host egress, applies rules, delivers
                // scheduled packets. Sim clock += TICK, matching tokio.
                scheduler.tick(guard_ref, TICK);

                if client_handle.is_finished() {
                    break client_handle.await.unwrap();
                }
            }
        });
        drop(guard);
        result
    }
}

impl Default for ClientServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps a host's future so every poll pins the thread-local
/// `current` to that host before the inner future runs. Without
/// this, tasks on a shared `LocalSet` would see whichever host was
/// set last — `sys()` lookups would land in the wrong kernel.
struct HostScoped<F> {
    id: HostId,
    inner: F,
}

impl<F> Future for HostScoped<F>
where
    F: Future + Unpin,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        crate::set_current(self.id);
        Pin::new(&mut self.inner).poll(cx)
    }
}
