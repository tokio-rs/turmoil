//! TCP state machine.
//!
//! v1 scope: handshake only. Sequence-number bookkeeping, send/recv
//! queues, FIN/RST, half-close, retransmit timers all come later.
//!
//! # Perf TODO
//! - `find_by_peer` and `count_children` sweep every socket via
//!   `SocketTable::iter`. Real kernels demux by 4-tuple hash. Replace
//!   with `HashMap<(local, remote), Fd>` on `SocketTable` before we
//!   take on TCP-heavy benchmarks — the old `turmoil` crate's
//!   per-packet overhead is partly the thing this rewrite is trying
//!   to beat.

use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use bytes::Bytes;

use crate::kernel::packet::{Packet, TcpFlags, TcpSegment, Transport};
use crate::kernel::socket::{Addr, BindKey, Domain, Fd, Socket, Tcb, TcpState, Type};
use crate::kernel::Kernel;

/// Drives `Kernel::poll_connect` for `SOCK_STREAM`. First poll builds a
/// TCB in `SynSent`, enqueues the SYN, and parks. Later polls observe
/// the TCB transitioning to `Established` (handshake done) and return
/// `Ready`.
pub(super) fn poll_connect(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    domain: Domain,
    peer: SocketAddr,
    is_bound: bool,
) -> Poll<Result<()>> {
    // Already in flight? Just check state and re-park.
    if let Some(tcb) = &k.lookup(fd).expect("fd validated").tcb {
        return match tcb.state {
            TcpState::Established => Poll::Ready(Ok(())),
            TcpState::SynSent | TcpState::SynReceived => {
                park_connect(k, fd, cx);
                Poll::Pending
            }
        };
    }

    // First poll: auto-bind if needed, build the TCB, emit SYN.
    if !is_bound {
        auto_bind(k, fd, domain, peer.ip())?;
    }
    let src = local_endpoint(k, fd);
    let isn = initial_sequence(k);
    {
        let st = k.lookup_mut(fd).expect("fd validated");
        st.tcb = Some(Tcb {
            state: TcpState::SynSent,
            peer,
            snd_nxt: isn.wrapping_add(1),
            rcv_nxt: 0,
        });
        st.peer = Some(Addr::Inet(peer));
    }
    emit(
        k,
        src,
        peer,
        TcpSegment {
            src_port: src.port(),
            dst_port: peer.port(),
            seq: isn,
            ack: 0,
            flags: TcpFlags {
                syn: true,
                ..TcpFlags::default()
            },
            window: DEFAULT_WINDOW,
            payload: Bytes::new(),
        },
    );
    park_connect(k, fd, cx);
    Poll::Pending
}

/// Inbound TCP dispatch. For v1: SYN → listener → SynReceived + SYN-ACK;
/// SYN-ACK → matching SynSent client → Established + ACK; ACK → matching
/// SynReceived server → Established + push to listener backlog.
pub(super) fn deliver(k: &mut Kernel, pkt: &Packet, s: &TcpSegment) {
    let local = SocketAddr::new(pkt.dst, s.dst_port);
    let remote = SocketAddr::new(pkt.src, s.src_port);

    // First try to demux to an established/in-progress connection by
    // 4-tuple. Listener fallback only runs if that misses.
    if let Some(fd) = find_by_peer(k, local, remote) {
        handle_on_connection(k, fd, local, remote, s);
        return;
    }

    // Listener fallback — SYN on an otherwise-unknown 4-tuple.
    if s.flags.syn && !s.flags.ack {
        if let Some(listener) = find_listener(k, local) {
            accept_syn(k, listener, local, remote, s);
        }
        // else: no listener → silently drop. RST-on-closed-port is a
        // later polish pass.
    }
}

fn handle_on_connection(
    k: &mut Kernel,
    fd: Fd,
    local: SocketAddr,
    remote: SocketAddr,
    s: &TcpSegment,
) {
    let state = k
        .lookup(fd)
        .expect("fd present")
        .tcb
        .as_ref()
        .expect("tcb present")
        .state;
    match state {
        // Client received SYN-ACK. Move to Established and ACK.
        TcpState::SynSent if s.flags.syn && s.flags.ack => {
            let (snd_nxt, rcv_nxt) = {
                let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
                tcb.state = TcpState::Established;
                tcb.rcv_nxt = s.seq.wrapping_add(1);
                (tcb.snd_nxt, tcb.rcv_nxt)
            };
            wake_connect(k, fd);
            emit(
                k,
                local,
                remote,
                TcpSegment {
                    src_port: local.port(),
                    dst_port: remote.port(),
                    seq: snd_nxt,
                    ack: rcv_nxt,
                    flags: TcpFlags {
                        ack: true,
                        ..TcpFlags::default()
                    },
                    window: DEFAULT_WINDOW,
                    payload: Bytes::new(),
                },
            );
        }
        // Server received ACK of its SYN-ACK. Promote to Established
        // and push onto the listener's ready queue.
        TcpState::SynReceived if s.flags.ack && !s.flags.syn => {
            let expected_ack = k.lookup(fd).unwrap().tcb.as_ref().unwrap().snd_nxt;
            if s.ack != expected_ack {
                return;
            }
            k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap().state = TcpState::Established;
            push_to_listener(k, fd, local);
        }
        _ => {
            // Anything else is out of scope for v1 (data, FIN, RST,
            // duplicate SYN). Drop silently.
        }
    }
}

fn accept_syn(
    k: &mut Kernel,
    listener_fd: Fd,
    local: SocketAddr,
    remote: SocketAddr,
    s: &TcpSegment,
) {
    // Check backlog capacity before we allocate anything.
    let (backlog, domain, ty) = {
        let st = k.lookup(listener_fd).expect("listener present");
        let listen = st.listen.as_ref().expect("listener has ListenState");
        (listen.backlog, st.domain, st.ty)
    };
    // Count in-progress (SynReceived) children + ready children
    // against backlog. Linux separates SYN backlog from accept backlog;
    // we collapse them for v1.
    let in_flight = count_children(k, listener_fd, local, remote);
    let ready = k
        .lookup(listener_fd)
        .unwrap()
        .listen
        .as_ref()
        .unwrap()
        .ready
        .len();
    if in_flight + ready >= backlog {
        return; // drop — client will perceive as SYN loss
    }

    let child = k.sockets.insert(Socket::new(domain, ty));
    let bind_key = BindKey {
        domain,
        ty,
        local_addr: local.ip(),
        local_port: local.port(),
    };
    // Server child shares the listener's tuple. insert_binding allows
    // multiple fds at one key (REUSEPORT); demux to an accepted child
    // runs by 4-tuple (find_by_peer) before listener fallback, so this
    // doesn't cross wires.
    k.sockets.insert_binding(bind_key.clone(), child);
    let isn = initial_sequence(k);
    {
        let st = k.sockets.get_mut(child).unwrap();
        st.bound = Some(bind_key);
        st.peer = Some(Addr::Inet(remote));
        st.tcb = Some(Tcb {
            state: TcpState::SynReceived,
            peer: remote,
            snd_nxt: isn.wrapping_add(1),
            rcv_nxt: s.seq.wrapping_add(1),
        });
    }
    emit(
        k,
        local,
        remote,
        TcpSegment {
            src_port: local.port(),
            dst_port: remote.port(),
            seq: isn,
            ack: s.seq.wrapping_add(1),
            flags: TcpFlags {
                syn: true,
                ack: true,
                ..TcpFlags::default()
            },
            window: DEFAULT_WINDOW,
            payload: Bytes::new(),
        },
    );
}

fn push_to_listener(k: &mut Kernel, child: Fd, local: SocketAddr) {
    // The listener shares `local` with the child. Find it.
    let Some(listener_fd) = find_listener(k, local) else {
        return;
    };
    let wakers: Vec<_> = {
        let listen = k
            .lookup_mut(listener_fd)
            .unwrap()
            .listen
            .as_mut()
            .expect("listener");
        listen.ready.push_back(child);
        listen.accept_wakers.drain(..).collect()
    };
    for w in wakers {
        w.wake();
    }
}

fn park_connect(k: &mut Kernel, fd: Fd, cx: &mut Context<'_>) {
    let st = k.lookup_mut(fd).expect("fd present");
    st.connect_waker = Some(cx.waker().clone());
}

fn wake_connect(k: &mut Kernel, fd: Fd) {
    if let Some(w) = k.lookup_mut(fd).unwrap().connect_waker.take() {
        w.wake();
    }
}

/// Find a non-listening socket whose `(bound local, tcb.peer)` matches.
fn find_by_peer(k: &Kernel, local: SocketAddr, remote: SocketAddr) -> Option<Fd> {
    k.sockets.iter().find_map(|(fd, s)| {
        let tcb = s.tcb.as_ref()?;
        if tcb.peer != remote {
            return None;
        }
        let bound = s.bound.as_ref()?;
        if bound.local_port != local.port() {
            return None;
        }
        if bound.local_addr == local.ip() || bound.local_addr.is_unspecified() {
            Some(fd)
        } else {
            None
        }
    })
}

/// Find a listening socket bound to `local` (or the matching wildcard).
fn find_listener(k: &Kernel, local: SocketAddr) -> Option<Fd> {
    let domain = match local {
        SocketAddr::V4(_) => Domain::Inet,
        SocketAddr::V6(_) => Domain::Inet6,
    };
    let exact = BindKey {
        domain,
        ty: Type::Stream,
        local_addr: local.ip(),
        local_port: local.port(),
    };
    let wildcard_ip = match local {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let wildcard = BindKey {
        domain,
        ty: Type::Stream,
        local_addr: wildcard_ip,
        local_port: local.port(),
    };
    for key in [&exact, &wildcard] {
        for &fd in k.sockets.find_by_bind(key) {
            if k.sockets.get(fd).unwrap().listen.is_some() {
                return Some(fd);
            }
        }
    }
    None
}

/// Count child sockets owned by `listener_fd`'s tuple that are still
/// handshaking (SynReceived) with `remote`.
fn count_children(k: &Kernel, listener_fd: Fd, local: SocketAddr, remote: SocketAddr) -> usize {
    k.sockets
        .iter()
        .filter(|(fd, s)| {
            *fd != listener_fd
                && s.tcb
                    .as_ref()
                    .map(|t| t.state == TcpState::SynReceived && t.peer == remote)
                    .unwrap_or(false)
                && s.bound
                    .as_ref()
                    .map(|b| b.local_port == local.port())
                    .unwrap_or(false)
        })
        .count()
}

fn emit(k: &mut Kernel, src: SocketAddr, dst: SocketAddr, seg: TcpSegment) {
    k.outbound.push_back(Packet {
        src: src.ip(),
        dst: dst.ip(),
        ttl: 64,
        payload: Transport::Tcp(seg),
    });
}

fn local_endpoint(k: &Kernel, fd: Fd) -> SocketAddr {
    let bind = k
        .lookup(fd)
        .unwrap()
        .bound
        .as_ref()
        .expect("fd bound by now");
    let ip = if bind.local_addr.is_unspecified() {
        // Pick a concrete source IP. For loopback peers the shim-side
        // bind uses 127.0.0.1/::1 already; wildcard binds only arise
        // from `0.0.0.0:0`. Match against the TCB peer.
        let peer = k.lookup(fd).unwrap().tcb.as_ref().map(|t| t.peer.ip());
        match peer {
            Some(p) if p.is_loopback() => match p {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
            },
            Some(p) => k
                .addresses
                .iter()
                .copied()
                .find(|a| a.is_ipv4() == p.is_ipv4())
                .unwrap_or(bind.local_addr),
            None => bind.local_addr,
        }
    } else {
        bind.local_addr
    };
    SocketAddr::new(ip, bind.local_port)
}

fn auto_bind(k: &mut Kernel, fd: Fd, domain: Domain, dst: IpAddr) -> Result<()> {
    let local_ip = if dst.is_loopback() {
        match dst {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        k.addresses
            .iter()
            .copied()
            .find(|a| a.is_ipv4() == dst.is_ipv4())
            .ok_or_else(|| Error::from(ErrorKind::AddrNotAvailable))?
    };
    let port = k
        .sockets
        .allocate_port(domain, Type::Stream)
        .ok_or_else(|| Error::from(ErrorKind::AddrInUse))?;
    let key = BindKey {
        domain,
        ty: Type::Stream,
        local_addr: local_ip,
        local_port: port,
    };
    k.sockets.insert_binding(key.clone(), fd);
    k.sockets.get_mut(fd).expect("fd present").bound = Some(key);
    Ok(())
}

fn initial_sequence(k: &mut Kernel) -> u32 {
    let v = k.tcp_isn;
    // Deterministic bump — real kernels randomize, but we want
    // reproducibility. Large step to keep successive connections'
    // sequence spaces from colliding visually.
    k.tcp_isn = k.tcp_isn.wrapping_add(0x1_0000);
    v
}

const DEFAULT_WINDOW: u16 = 65535;
