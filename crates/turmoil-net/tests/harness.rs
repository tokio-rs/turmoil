//! Primitives for harnesses that own scheduling: parking the `Net`
//! between steps, per-host egress and delivery, host resets, and
//! name resolution hooks.

use std::future::Future;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio::task::{JoinHandle, LocalSet};
use turmoil_net::fixture;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream, UdpSocket};
use turmoil_net::{HostId, KernelConfig, Net, Packet, Transport, UdpDatagram};

const A: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
const B: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

/// Steps hosts on one `LocalSet`, installing the `Net` only while
/// tasks run, and routes packets itself between steps.
struct Harness {
    rt: Runtime,
    set: LocalSet,
    net: Option<Net>,
}

impl Harness {
    fn new(net: Net) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        Self {
            rt,
            set: LocalSet::new(),
            net: Some(net),
        }
    }

    fn net(&mut self) -> &mut Net {
        self.net.as_mut().unwrap()
    }

    fn spawn<F: Future + 'static>(&mut self, host: HostId, fut: F) -> JoinHandle<F::Output> {
        self.set.spawn_local(Scoped {
            host,
            inner: Box::pin(fut),
        })
    }

    /// Run tasks to idle with the `Net` installed, then park it and
    /// route everything the hosts sent.
    fn step(&mut self) {
        let guard = self.net.take().unwrap().enter();
        self.rt.block_on(async {
            self.set
                .run_until(tokio::time::sleep(Duration::from_millis(1)))
                .await
        });
        let mut net = guard.exit();
        let hosts: Vec<_> = net.host_ids().collect();
        let mut out = Vec::new();
        for host in hosts {
            net.egress(host, &mut out);
        }
        for pkt in out {
            for to in net.receivers(&pkt) {
                net.deliver_to(to, pkt.clone());
            }
        }
        self.net = Some(net);
    }

    fn run_until<T>(&mut self, handle: &JoinHandle<T>) {
        for _ in 0..1000 {
            if handle.is_finished() {
                return;
            }
            self.step();
        }
        panic!("task did not finish");
    }

    fn join<T>(&mut self, handle: JoinHandle<T>) -> T {
        self.run_until(&handle);
        self.rt.block_on(handle).unwrap()
    }
}

struct Scoped<F> {
    host: HostId,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        turmoil_net::set_current(self.host);
        self.inner.as_mut().poll(cx)
    }
}

#[test]
fn parked_net_carries_connections_across_steps() {
    let mut net = Net::new();
    let a = net.add_host(A);
    let b = net.add_host(B);
    let mut h = Harness::new(net);

    h.spawn(b, async {
        let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0; 5];
        sock.read_exact(&mut buf).await.unwrap();
        sock.write_all(&buf).await.unwrap();
    });
    let client = h.spawn(a, async {
        let mut sock = TcpStream::connect((B, 9000)).await.unwrap();
        sock.write_all(b"hello").await.unwrap();
        let mut buf = [0; 5];
        sock.read_exact(&mut buf).await.unwrap();
        buf
    });

    assert_eq!(&h.join(client), b"hello");
}

#[test]
fn reset_host_discards_sockets_silently() {
    let mut net = Net::new();
    let a = net.add_host(A);
    let b = net.add_host(B);
    let mut h = Harness::new(net);

    let accepted = Rc::new(tokio::sync::Notify::new());
    let notify = accepted.clone();
    // Keep the accepted stream alive so only the reset can tear it down.
    h.spawn(b, async move {
        let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
        let (_sock, _) = listener.accept().await.unwrap();
        notify.notify_one();
        std::future::pending::<()>().await;
    });
    let connect = h.spawn(a, async { TcpStream::connect((B, 9000)).await.unwrap() });
    let stream = h.join(connect);
    let accepted = h.spawn(a, async move { accepted.notified().await });
    h.join(accepted);

    h.net().reset_host(b);
    let mut out = Vec::new();
    h.net().egress(b, &mut out);
    assert!(out.is_empty(), "reset emitted {out:?}");
    assert!(h.net().netstat(b).entries.is_empty());

    // The rebooted host answers the stale connection with a RST.
    let write = h.spawn(a, async move {
        let mut stream = stream;
        stream.write_all(b"x").await.unwrap();
        stream.read(&mut [0; 1]).await.unwrap_err().kind()
    });
    assert_eq!(h.join(write), ErrorKind::ConnectionReset);
}

#[test]
fn resolver_runs_before_builtin_dns() {
    let mut net = Net::new();
    let a = net.add_host(A);
    let b = net.add_host(B);
    net.set_resolver(|name| (name == "db").then_some(B));
    let mut h = Harness::new(net);

    h.spawn(b, async {
        let sock = UdpSocket::bind("0.0.0.0:9000").await.unwrap();
        let mut buf = [0; 2];
        let (_, from) = sock.recv_from(&mut buf).await.unwrap();
        sock.send_to(&buf, from).await.unwrap();
    });
    let client = h.spawn(a, async {
        assert_eq!(turmoil_net::lookup_host("db"), Some(B));
        let sock = UdpSocket::bind("0.0.0.0:0").await.unwrap();
        sock.send_to(b"hi", "db:9000").await.unwrap();
        let mut buf = [0; 2];
        sock.recv_from(&mut buf).await.unwrap();
        buf
    });

    assert_eq!(&h.join(client), b"hi");
}

#[test]
fn cast_receivers_exclude_sender() {
    let mut net = Net::new();
    let a = net.add_host(A);
    let b = net.add_host(B);
    let c = net.add_host(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)));

    let pkt = |dst: IpAddr| Packet {
        src: A,
        dst,
        ttl: 1,
        payload: Transport::Udp(UdpDatagram {
            src_port: 1,
            dst_port: 2,
            payload: Bytes::new(),
        }),
    };
    assert_eq!(net.receivers(&pkt(Ipv4Addr::BROADCAST.into())), [b, c]);
    assert_eq!(net.receivers(&pkt(B)), [b]);
    assert_eq!(net.receivers(&pkt(A)), [a]);
    assert!(net
        .receivers(&pkt(IpAddr::V4(Ipv4Addr::new(10, 9, 9, 9))))
        .is_empty());
}

#[test]
fn sockets_dropped_after_exit_do_not_panic() {
    let mut net = Net::new();
    net.add_host(A);
    let guard = net.enter();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let sock = rt.block_on(UdpSocket::bind("0.0.0.0:9000")).unwrap();
    let net = guard.exit();
    drop(sock);
    drop(net);
}

#[test]
fn ephemeral_port_range_is_configurable() {
    let cfg = KernelConfig::default().ephemeral_ports(50000..=50000);
    fixture::lo_with_config(cfg, async {
        let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert_eq!(first.local_addr().unwrap().port(), 50000);
        let err = UdpSocket::bind("127.0.0.1:0").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
    });
}

#[test]
fn udp_receive_queue_drops_when_full() {
    let cfg = KernelConfig::default().udp_recv_queue_cap(1);
    fixture::lo_with_config(cfg, async {
        let server = UdpSocket::bind("127.0.0.1:9000").await.unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(b"1", "127.0.0.1:9000").await.unwrap();
        client.send_to(b"2", "127.0.0.1:9000").await.unwrap();

        let mut buf = [0; 1];
        server.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf, b"1");
        let next = tokio::time::timeout(Duration::from_secs(1), server.recv_from(&mut buf)).await;
        assert!(next.is_err(), "second datagram should have been dropped");
    });
}
