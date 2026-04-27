//! TCP state machine.
//!
//! v1 scope: handshake + streaming data. FIN/RST, half-close, and
//! retransmit still come later — the simulated fabric is reliable, so
//! dropping retransmit for now is safe.
//!
//! # Perf TODO
//! - `find_by_peer`, `count_children`, and `segment_all` sweep every
//!   socket via `SocketTable::iter`. Real kernels demux by 4-tuple
//!   hash. Replace with `HashMap<(local, remote), Fd>` on `SocketTable`
//!   before we take on TCP-heavy benchmarks — the old `turmoil` crate's
//!   per-packet overhead is partly the thing this rewrite is trying
//!   to beat.

use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};

use crate::kernel::packet::{self, Packet, TcpFlags, TcpSegment, Transport};
use crate::kernel::socket::{
    Addr, BindKey, Domain, Fd, Socket, Tcb, TcpState, Type, RECV_BUF_CAP, SEND_BUF_CAP,
};
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
            snd_una: isn.wrapping_add(1),
            snd_wnd: DEFAULT_WINDOW,
            rcv_nxt: 0,
            send_buf: BytesMut::new(),
            recv_buf: BytesMut::new(),
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
            let (snd_nxt, rcv_nxt, window) = {
                let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
                tcb.state = TcpState::Established;
                tcb.rcv_nxt = s.seq.wrapping_add(1);
                tcb.snd_wnd = s.window;
                (tcb.snd_nxt, tcb.rcv_nxt, advertised_window(0))
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
                    window,
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
            {
                let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
                tcb.state = TcpState::Established;
                tcb.snd_wnd = s.window;
            }
            push_to_listener(k, fd, local);
        }
        // Data / ACK on an open connection.
        TcpState::Established => handle_established(k, fd, local, remote, s),
        _ => {
            // Anything else is out of scope for v1 (FIN, RST,
            // duplicate SYN). Drop silently.
        }
    }
}

/// ACK processing, in-order data receipt, and waker plumbing. Out-of-
/// order segments are dropped — v1's fabric is reliable.
fn handle_established(
    k: &mut Kernel,
    fd: Fd,
    local: SocketAddr,
    remote: SocketAddr,
    s: &TcpSegment,
) {
    let mut write_waker = None;
    let mut read_waker = None;
    let mut send_ack = false;

    {
        let st = k.lookup_mut(fd).unwrap();
        let tcb = st.tcb.as_mut().unwrap();

        // ACK: drain ACK'd bytes from send_buf, refresh advertised
        // window. Any ACK is worth waking a parked writer for — either
        // bytes drained or the window grew.
        if s.flags.ack {
            let acked = s.ack.wrapping_sub(tcb.snd_una);
            let in_flight = tcb.snd_nxt.wrapping_sub(tcb.snd_una);
            if acked > 0 && acked <= in_flight {
                let _ = tcb.send_buf.split_to(acked as usize);
                tcb.snd_una = s.ack;
            }
            tcb.snd_wnd = s.window;
            write_waker = st.write_waker.take();
        }

        // Data: accept if it lands exactly at rcv_nxt and fits under
        // the receive cap. Gaps, overlaps, and overruns all drop.
        let tcb = st.tcb.as_mut().unwrap();
        if !s.payload.is_empty() && s.seq == tcb.rcv_nxt {
            let room = RECV_BUF_CAP.saturating_sub(tcb.recv_buf.len());
            let n = s.payload.len().min(room);
            if n > 0 {
                tcb.recv_buf.extend_from_slice(&s.payload[..n]);
                tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(n as u32);
                read_waker = st.read_waker.take();
                send_ack = true;
            }
        }
    }

    if let Some(w) = write_waker {
        w.wake();
    }
    if let Some(w) = read_waker {
        w.wake();
    }

    if send_ack {
        let (snd_nxt, rcv_nxt, window) = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            (
                tcb.snd_nxt,
                tcb.rcv_nxt,
                advertised_window(tcb.recv_buf.len()),
            )
        };
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
                window,
                payload: Bytes::new(),
            },
        );
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
            snd_una: isn.wrapping_add(1),
            snd_wnd: s.window,
            rcv_nxt: s.seq.wrapping_add(1),
            send_buf: BytesMut::new(),
            recv_buf: BytesMut::new(),
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

/// Copy bytes into the socket's `send_buf`, up to the remaining
/// capacity. Returns `Pending` (parking `write_waker`) when the buffer
/// is already full. Segmentation and the actual wire emit happen later,
/// in `segment_all` during `egress`.
pub(super) fn poll_write(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &[u8],
) -> Poll<Result<usize>> {
    let st = match k.lookup_mut(fd) {
        Ok(st) => st,
        Err(e) => return Poll::Ready(Err(e)),
    };
    let Some(tcb) = st.tcb.as_ref() else {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    };
    if tcb.state != TcpState::Established {
        // Mid-handshake writes could be spec-legal in some states, but
        // for v1 we only allow writes on an open connection.
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    }
    let space = SEND_BUF_CAP.saturating_sub(tcb.send_buf.len());
    if space == 0 {
        st.write_waker = Some(cx.waker().clone());
        return Poll::Pending;
    }
    let n = buf.len().min(space);
    st.tcb
        .as_mut()
        .unwrap()
        .send_buf
        .extend_from_slice(&buf[..n]);
    Poll::Ready(Ok(n))
}

/// Drain bytes from the socket's `recv_buf` into `buf`. Returns
/// `Pending` (parking `read_waker`) when the buffer is empty. An ACK
/// is emitted afterward if the drain opened enough window to be worth
/// advertising — avoids silly-window syndrome on tiny reads.
pub(super) fn poll_read(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &mut [u8],
) -> Poll<Result<usize>> {
    let (n, should_update_window, local, remote) = {
        let st = match k.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        // Inspect without holding a borrow across the mutable ops.
        let (empty, established, peer) = match st.tcb.as_ref() {
            None => return Poll::Ready(Err(Error::from(ErrorKind::NotConnected))),
            Some(t) => (
                t.recv_buf.is_empty(),
                t.state == TcpState::Established,
                t.peer,
            ),
        };
        if empty {
            if !established {
                return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
            }
            st.read_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let local = bound_endpoint(st);
        let tcb = st.tcb.as_mut().unwrap();
        let n = tcb.recv_buf.len().min(buf.len());
        let drained = tcb.recv_buf.split_to(n);
        buf[..n].copy_from_slice(&drained);
        // Window-update trigger: if we freed ≥ half the recv cap,
        // advertise. Crude SWS avoidance; refine alongside real flow
        // control.
        let should_update = n >= RECV_BUF_CAP / 2;
        (n, should_update, local, peer)
    };

    if should_update_window {
        let (snd_nxt, rcv_nxt, window) = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            (
                tcb.snd_nxt,
                tcb.rcv_nxt,
                advertised_window(tcb.recv_buf.len()),
            )
        };
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
                window,
                payload: Bytes::new(),
            },
        );
    }

    Poll::Ready(Ok(n))
}

/// Sweep all established TCP sockets and chop queued bytes (the tail of
/// `send_buf` past `snd_nxt - snd_una`) into MSS-sized segments bounded
/// by the peer's advertised window. Invoked by `egress` before the
/// outbound drain, so newly-segmented packets ride the same pump that
/// handles UDP and handshake traffic.
pub(super) fn segment_all(k: &mut Kernel) {
    let candidates: Vec<Fd> = k
        .sockets
        .iter()
        .filter(|(_, s)| {
            s.tcb
                .as_ref()
                .map(|t| {
                    t.state == TcpState::Established
                        && t.send_buf.len() > (t.snd_nxt.wrapping_sub(t.snd_una)) as usize
                })
                .unwrap_or(false)
        })
        .map(|(fd, _)| fd)
        .collect();

    for fd in candidates {
        segment_one(k, fd);
    }
}

fn segment_one(k: &mut Kernel, fd: Fd) {
    let local = {
        let st = k.lookup(fd).unwrap();
        bound_endpoint(st)
    };
    let mss = mss_for(k, local.ip());

    loop {
        let (seq, payload) = {
            let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
            let in_flight = tcb.snd_nxt.wrapping_sub(tcb.snd_una) as usize;
            let unsent = tcb.send_buf.len().saturating_sub(in_flight);
            if unsent == 0 {
                return;
            }
            let wnd_remaining = (tcb.snd_wnd as usize).saturating_sub(in_flight);
            if wnd_remaining == 0 {
                return;
            }
            let n = unsent.min(mss).min(wnd_remaining);
            let start = in_flight;
            let end = start + n;
            let payload = Bytes::copy_from_slice(&tcb.send_buf[start..end]);
            let seq = tcb.snd_nxt;
            tcb.snd_nxt = tcb.snd_nxt.wrapping_add(n as u32);
            (seq, payload)
        };
        let remote = k.lookup(fd).unwrap().tcb.as_ref().unwrap().peer;
        let window = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            advertised_window(tcb.recv_buf.len())
        };
        emit(
            k,
            local,
            remote,
            TcpSegment {
                src_port: local.port(),
                dst_port: remote.port(),
                seq,
                ack: k.lookup(fd).unwrap().tcb.as_ref().unwrap().rcv_nxt,
                flags: TcpFlags {
                    ack: true,
                    psh: true,
                    ..TcpFlags::default()
                },
                window,
                payload,
            },
        );
    }
}

/// Maximum TCP payload for a segment leaving `src_ip`. Mirrors the UDP
/// MTU math: pick the right MTU (loopback vs. external), subtract IP +
/// TCP headers.
fn mss_for(k: &Kernel, src_ip: IpAddr) -> usize {
    let ip_hdr = match src_ip {
        IpAddr::V4(_) => packet::IPV4_HEADER_SIZE as u32,
        IpAddr::V6(_) => packet::IPV6_HEADER_SIZE as u32,
    };
    let mtu = if src_ip.is_loopback() {
        k.loopback_mtu
    } else {
        k.mtu
    };
    mtu.saturating_sub(ip_hdr)
        .saturating_sub(packet::TCP_HEADER_SIZE as u32) as usize
}

/// Pull the local endpoint for an established socket — wildcard binds
/// never reach here (the handshake path concretizes the source IP
/// before inserting into the TCB).
fn bound_endpoint(st: &Socket) -> SocketAddr {
    let bind = st.bound.as_ref().expect("bound at handshake time");
    SocketAddr::new(bind.local_addr, bind.local_port)
}

fn advertised_window(recv_buf_len: usize) -> u16 {
    RECV_BUF_CAP
        .saturating_sub(recv_buf_len)
        .min(u16::MAX as usize) as u16
}

const DEFAULT_WINDOW: u16 = 65535;
