//! Multi-host client/server fixture.

use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::Poll;

use tokio::task::LocalSet;

use crate::{HostId, Net, ToIpAddrs};

type BoxFut = Pin<Box<dyn Future<Output = ()>>>;

/// Multi-host fixture with N servers plus one client. Each role runs
/// as its own host on its own [`LocalSet`]; the fabric is stepped
/// between turns. The run finishes the moment the client's future
/// resolves — any server futures still running are aborted.
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
        let client_id = net.add_host(addrs);
        let guard = net.enter();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current_thread runtime");

        let result = rt.block_on(async move {
            // One LocalSet per host. Each server's future is spawned
            // onto its own set; the client's future we drive ourselves
            // so we can observe completion.
            let server_sets: Vec<(HostId, LocalSet)> = servers
                .into_iter()
                .map(|(id, fut)| {
                    let set = LocalSet::new();
                    set.spawn_local(fut);
                    (id, set)
                })
                .collect();
            let client_set = LocalSet::new();
            let mut client_fut = Box::pin(fut);

            loop {
                crate::CURRENT.with(|c| {
                    c.borrow_mut()
                        .as_mut()
                        .expect("guard is live")
                        .fabric
                        .step();
                });

                for (id, set) in &server_sets {
                    crate::CURRENT.with(|c| {
                        c.borrow_mut().as_mut().expect("guard is live").current = Some(*id);
                    });
                    set.run_until(tokio::task::yield_now()).await;
                }

                crate::CURRENT.with(|c| {
                    c.borrow_mut().as_mut().expect("guard is live").current = Some(client_id);
                });
                let completed = client_set
                    .run_until(async {
                        let mut out = None;
                        poll_fn(|cx| match client_fut.as_mut().poll(cx) {
                            Poll::Ready(v) => {
                                out = Some(v);
                                Poll::Ready(())
                            }
                            Poll::Pending => Poll::Ready(()),
                        })
                        .await;
                        out
                    })
                    .await;
                if let Some(out) = completed {
                    return out;
                }
                tokio::task::yield_now().await;
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
