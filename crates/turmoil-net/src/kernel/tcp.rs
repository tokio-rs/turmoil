//! TCP state machine.
//!
//! Handshake, streaming data, FIN/RST, half-close, and go-back-N
//! retransmit. Retransmit is count-based — see [`check_retx`] — so
//! the mechanism stays harness-agnostic; time-driven RTO with
//! SRTT/RTTVAR is a later concern once flow/congestion control is on
//! the table.
//!
//! ## Go-back-N, not SACK
//!
//! On retransmit we rewind `snd_nxt = snd_una` and re-emit the entire
//! unacked window. Real TCP typically negotiates SACK and resends
//! only the gaps. We don't, for two reasons:
//!
//! - **No reassembly on receive.** Our receiver accepts segments
//!   strictly in order (`seq == rcv_nxt`, else drop). Selective retx
//!   would only pay off if the receiver buffered out-of-order
//!   segments; without that, resending the tail is necessary anyway.
//! - **Simpler state.** No per-segment tracking, no retx queue, no
//!   SACK option parsing.
//!
//! App-visible behavior is identical: bytes arrive in-order and
//! intact. The cost is wire inefficiency under loss — a test that
//! counts packets could tell the difference, but the socket API
//! can't.
//!
//! # Perf TODO
//! - `segment_all` still sweeps every socket via `SocketTable::iter`
//!   each egress to find ones with transmittable bytes. A dirty-set
//!   (fd pushed whenever send_buf grows or a FIN becomes pending) would
//!   cut that to an O(writers) scan.

use std::io::{Error, ErrorKind, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};

use crate::kernel::packet::{self, Packet, TcpFlags, TcpSegment, Transport};
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
            // Close mid-connect: SYN retx exhaustion surfaces as
            // TimedOut (Linux's ETIMEDOUT); anything else is the
            // peer RST'ing, which Linux reports as ECONNREFUSED.
            TcpState::Closed
            | TcpState::FinWait1
            | TcpState::FinWait2
            | TcpState::CloseWait
            | TcpState::LastAck
            | TcpState::Closing => {
                if tcb.timed_out {
                    Poll::Ready(Err(Error::from(ErrorKind::TimedOut)))
                } else {
                    Poll::Ready(Err(Error::from(ErrorKind::ConnectionRefused)))
                }
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
            wr_closed: false,
            peer_fin: false,
            fin_seq: None,
            reset: false,
            timed_out: false,
            egress_since_ack: 0,
            retx_attempts: 0,
        });
        st.peer = Some(Addr::Inet(peer));
    }
    k.sockets.insert_connection(src, peer, fd);
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

/// Inbound TCP dispatch.
pub(super) fn deliver(k: &mut Kernel, pkt: &Packet, s: &TcpSegment) {
    let local = SocketAddr::new(pkt.dst, s.dst_port);
    let remote = SocketAddr::new(pkt.src, s.src_port);

    // First try to demux to an established/in-progress connection by
    // 4-tuple. Listener fallback only runs if that misses.
    if let Some(fd) = k.sockets.find_connection(local, remote) {
        handle_on_connection(k, fd, local, remote, s);
        return;
    }

    // Listener fallback — SYN on an otherwise-unknown 4-tuple.
    if s.flags.syn && !s.flags.ack {
        if let Some(listener) = find_listener(k, local) {
            accept_syn(k, listener, local, remote, s);
            return;
        }
        // No listener at this port — reply with RST so the peer sees
        // `ConnectionRefused` instead of timing out.
        emit_rst(k, local, remote, s);
        return;
    }

    // Non-SYN to an unknown 4-tuple. RFC 793 says RST, unless the
    // segment is itself a RST (that would be infinite ping-pong).
    if !s.flags.rst {
        emit_rst(k, local, remote, s);
    }
}

/// Emit a RST in response to `s`. Follows RFC 793's ACK/SEQ rules:
/// if the offending segment carried ACK, reflect `ack` as our `seq`
/// and leave the RST unacked; otherwise set `ack = seq + segment_len`
/// and flag ACK.
fn emit_rst(k: &mut Kernel, local: SocketAddr, remote: SocketAddr, s: &TcpSegment) {
    let (seq, ack, ack_flag) = if s.flags.ack {
        (s.ack, 0, false)
    } else {
        let seg_len = s.payload.len() as u32
            + if s.flags.syn { 1 } else { 0 }
            + if s.flags.fin { 1 } else { 0 };
        (0, s.seq.wrapping_add(seg_len), true)
    };
    emit(
        k,
        local,
        remote,
        TcpSegment {
            src_port: local.port(),
            dst_port: remote.port(),
            seq,
            ack,
            flags: TcpFlags {
                rst: true,
                ack: ack_flag,
                ..TcpFlags::default()
            },
            window: 0,
            payload: Bytes::new(),
        },
    );
}

fn handle_on_connection(
    k: &mut Kernel,
    fd: Fd,
    local: SocketAddr,
    remote: SocketAddr,
    s: &TcpSegment,
) {
    // RST trumps all other processing. Tear the connection down and
    // wake every parked task with ConnectionReset.
    if s.flags.rst {
        abort_connection(k, fd);
        return;
    }

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
            let recv_cap = k.recv_buf_cap;
            let (snd_nxt, rcv_nxt, window) = {
                let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
                tcb.state = TcpState::Established;
                tcb.rcv_nxt = s.seq.wrapping_add(1);
                tcb.snd_wnd = s.window;
                (tcb.snd_nxt, tcb.rcv_nxt, advertised_window(recv_cap, 0))
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
        // Data / ACK / FIN on an open or half-closed connection.
        TcpState::Established
        | TcpState::FinWait1
        | TcpState::FinWait2
        | TcpState::CloseWait
        | TcpState::Closing
        | TcpState::LastAck => handle_established(k, fd, local, remote, s),
        TcpState::Closed => {
            // Socket is torn down but the TCB lingers until the shim
            // drops its Fd. Ignore any late inbound traffic.
        }
        TcpState::SynSent | TcpState::SynReceived => {
            // Handshake still in progress but segment doesn't match
            // the expected SYN-ACK / ACK. Drop for v1 — RST-on-
            // unexpected is a polish pass.
        }
    }
}

/// ACK processing, in-order data receipt, FIN handling, and waker
/// plumbing. Out-of-order segments are dropped — v1's fabric is
/// reliable.
fn handle_established(
    k: &mut Kernel,
    fd: Fd,
    local: SocketAddr,
    remote: SocketAddr,
    s: &TcpSegment,
) {
    let mut wake_write = false;
    let mut wake_read = false;
    let mut send_ack = false;
    let recv_cap = k.recv_buf_cap;

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
                // FIN (if sent) sits at `fin_seq` and consumes one seq
                // past the data. Don't try to drain buffer bytes for
                // the FIN's byte.
                let fin_acked = tcb
                    .fin_seq
                    .map(|fs| s.ack == fs.wrapping_add(1))
                    .unwrap_or(false);
                let data_bytes = if fin_acked { acked - 1 } else { acked };
                if data_bytes > 0 {
                    let _ = tcb.send_buf.split_to(data_bytes as usize);
                }
                tcb.snd_una = s.ack;
                // Progress — retx machinery resets.
                tcb.egress_since_ack = 0;
                tcb.retx_attempts = 0;

                // Advance state machine on FIN-ACK.
                if fin_acked {
                    tcb.state = match tcb.state {
                        TcpState::FinWait1 => TcpState::FinWait2,
                        TcpState::Closing => TcpState::Closed,
                        TcpState::LastAck => TcpState::Closed,
                        other => other,
                    };
                }
            }
            tcb.snd_wnd = s.window;
            wake_write = true;
        }

        // Data: accept if it lands exactly at rcv_nxt and fits under
        // the receive cap. Gaps, overlaps, and overruns all drop.
        let tcb = st.tcb.as_mut().unwrap();
        if !s.payload.is_empty() && s.seq == tcb.rcv_nxt && !tcb.peer_fin {
            let room = recv_cap.saturating_sub(tcb.recv_buf.len());
            let n = s.payload.len().min(room);
            if n > 0 {
                tcb.recv_buf.extend_from_slice(&s.payload[..n]);
                tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(n as u32);
                wake_read = true;
                send_ack = true;
            }
        }

        // FIN: consumes one sequence number right after any payload.
        // Accept only if it lands in order.
        let tcb = st.tcb.as_mut().unwrap();
        if s.flags.fin && !tcb.peer_fin {
            let fin_seq = s.seq.wrapping_add(s.payload.len() as u32);
            if fin_seq == tcb.rcv_nxt {
                tcb.peer_fin = true;
                tcb.rcv_nxt = tcb.rcv_nxt.wrapping_add(1);
                send_ack = true;
                // Wake any parked reader so it can observe EOF once
                // the buffer drains.
                wake_read = true;
                let tcb = st.tcb.as_mut().unwrap();
                tcb.state = match tcb.state {
                    TcpState::Established => TcpState::CloseWait,
                    TcpState::FinWait1 => TcpState::Closing,
                    TcpState::FinWait2 => TcpState::Closed,
                    other => other,
                };
            }
        }

        if wake_write {
            st.wake_write();
        }
        if wake_read {
            st.wake_read();
        }
    }

    if send_ack {
        let (snd_nxt, rcv_nxt, window) = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            (
                tcb.snd_nxt,
                tcb.rcv_nxt,
                advertised_window(recv_cap, tcb.recv_buf.len()),
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
    let in_flight = count_children(k, listener_fd, local);
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
    // runs by 4-tuple (find_connection) before listener fallback, so
    // this doesn't cross wires.
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
            wr_closed: false,
            peer_fin: false,
            fin_seq: None,
            reset: false,
            timed_out: false,
            egress_since_ack: 0,
            retx_attempts: 0,
        });
    }
    k.sockets.insert_connection(local, remote, child);
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

/// Decide how to close a TCP socket being dropped by the shim. Returns
/// `true` if the caller should reap the table entry immediately; `false`
/// means "leave the entry alive so the kernel can keep draining queued
/// bytes / FIN / ACKs, reap later via `reap_closed`".
///
/// RST-worthy cases (unread bytes, or dropping with a live write side)
/// get an immediate RST and are reaped here. Clean drops on a
/// still-active connection queue FIN (if not already) and leave the
/// TCB alive through its close states. Non-TCP / listener / already
/// terminal sockets are reaped immediately.
pub(super) fn on_close(k: &mut Kernel, fd: Fd) -> bool {
    enum Action {
        Reap,
        Linger,
        Rst {
            local: SocketAddr,
            remote: SocketAddr,
        },
        CloseListener {
            local: SocketAddr,
        },
    }

    let action = {
        let Some(st) = k.sockets.get(fd) else {
            return true;
        };
        match (st.ty, st.tcb.as_ref(), st.listen.as_ref()) {
            (Type::Stream, None, Some(_)) => {
                // Listener being closed — RST any unaccepted children
                // (both accept-ready and still-handshaking) before
                // reaping the listener itself.
                Action::CloseListener {
                    local: bound_endpoint(st),
                }
            }
            (Type::Stream, Some(tcb), _)
                if !tcb.reset
                    && !tcb.timed_out
                    && tcb.state != TcpState::Closed
                    && tcb.state != TcpState::SynSent
                    && tcb.state != TcpState::SynReceived =>
            {
                if !tcb.recv_buf.is_empty() {
                    // Unread bytes on drop → RST (Linux behavior).
                    Action::Rst {
                        local: bound_endpoint(st),
                        remote: tcb.peer,
                    }
                } else {
                    // Clean drop: let TCP finish closing in the
                    // background. Our helper path below queues FIN if
                    // the app never called shutdown.
                    Action::Linger
                }
            }
            _ => Action::Reap,
        }
    };

    match action {
        Action::Reap => true,
        Action::Rst { local, remote } => {
            let (seq, ack) = {
                let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
                (tcb.snd_nxt, tcb.rcv_nxt)
            };
            emit(
                k,
                local,
                remote,
                TcpSegment {
                    src_port: local.port(),
                    dst_port: remote.port(),
                    seq,
                    ack,
                    flags: TcpFlags {
                        rst: true,
                        ack: true,
                        ..TcpFlags::default()
                    },
                    window: 0,
                    payload: Bytes::new(),
                },
            );
            true
        }
        Action::Linger => {
            // Mark the socket as fd-closed and, if the app hasn't
            // shut the write side yet, queue a FIN. The TCB stays in
            // the table so egress / deliver can finish the close
            // handshake. `reap_closed` reaps once TCP reaches a
            // terminal state.
            let st = k.sockets.get_mut(fd).unwrap();
            st.fd_closed = true;
            let tcb = st.tcb.as_mut().unwrap();
            if !tcb.wr_closed {
                let fin_seq = tcb.snd_una.wrapping_add(tcb.send_buf.len() as u32);
                tcb.fin_seq = Some(fin_seq);
                tcb.wr_closed = true;
                tcb.state = match tcb.state {
                    TcpState::Established => TcpState::FinWait1,
                    TcpState::CloseWait => TcpState::LastAck,
                    other => other,
                };
            }
            false
        }
        Action::CloseListener { local } => {
            // Collect every unaccepted child and RST each. Real Linux
            // does this synchronously on listener destroy.
            //
            // Unaccepted = sitting in `listen.ready` (established but
            // not yet handed to the app) OR still handshaking in
            // SynReceived. Accepted children own their own lifecycle
            // via the user's TcpStream.
            let listener_port = local.port();
            let wildcard = local.ip().is_unspecified();
            let mut children: Vec<Fd> = k
                .sockets
                .get(fd)
                .and_then(|s| s.listen.as_ref())
                .map(|l| l.ready.iter().copied().collect())
                .unwrap_or_default();
            for (child_fd, st) in k.sockets.iter() {
                if child_fd == fd || children.contains(&child_fd) {
                    continue;
                }
                let (Some(tcb), Some(bind)) = (st.tcb.as_ref(), st.bound.as_ref()) else {
                    continue;
                };
                if tcb.state != TcpState::SynReceived {
                    continue;
                }
                if bind.local_port != listener_port {
                    continue;
                }
                if !wildcard && bind.local_addr != local.ip() {
                    continue;
                }
                children.push(child_fd);
            }
            for child in children {
                let Some(st) = k.sockets.get(child) else {
                    continue;
                };
                let Some(tcb) = st.tcb.as_ref() else {
                    k.sockets.remove(child);
                    continue;
                };
                let child_local = bound_endpoint(st);
                let (seq, ack, peer) = (tcb.snd_nxt, tcb.rcv_nxt, tcb.peer);
                emit(
                    k,
                    child_local,
                    peer,
                    TcpSegment {
                        src_port: child_local.port(),
                        dst_port: peer.port(),
                        seq,
                        ack,
                        flags: TcpFlags {
                            rst: true,
                            ack: true,
                            ..TcpFlags::default()
                        },
                        window: 0,
                        payload: Bytes::new(),
                    },
                );
                k.sockets.remove(child);
            }
            true
        }
    }
}

/// Reap any sockets the shim has dropped that have now reached a
/// terminal TCP state. Called at the end of each `egress` pass.
pub(super) fn reap_closed(k: &mut Kernel) {
    let victims: Vec<Fd> = k
        .sockets
        .iter()
        .filter(|(_, s)| {
            s.fd_closed
                && s.tcb
                    .as_ref()
                    .map(|t| t.state == TcpState::Closed || t.reset)
                    .unwrap_or(true)
        })
        .map(|(fd, _)| fd)
        .collect();
    for fd in victims {
        k.sockets.remove(fd);
    }
}

/// Mark a connection as aborted. Buffers are cleared (post-RST reads
/// should see the error, not stale data), state moves to `Closed`,
/// `reset` is set so subsequent ops return `ConnectionReset`, and all
/// parked tasks wake so they can observe the new state.
fn abort_connection(k: &mut Kernel, fd: Fd) {
    abort_with(k, fd, AbortReason::Reset);
}

/// Abort via retransmit exhaustion. Same flush/wake as
/// [`abort_connection`] but surfaces as `TimedOut` rather than
/// `ConnectionReset`, matching Linux's `ETIMEDOUT` from
/// `tcp_retries2`. No RST is emitted — exhaustion means packets
/// weren't reaching the peer, so another RST would fare no better.
fn abort_timed_out(k: &mut Kernel, fd: Fd) {
    abort_with(k, fd, AbortReason::TimedOut);
}

enum AbortReason {
    Reset,
    TimedOut,
}

/// Returns the post-abort error a syscall should surface, if any.
/// `reset` wins over `timed_out` when both somehow land — RST is the
/// stronger signal — but in practice only one ever fires per TCB.
fn abort_error(tcb: &Tcb) -> Option<Error> {
    if tcb.reset {
        Some(Error::from(ErrorKind::ConnectionReset))
    } else if tcb.timed_out {
        Some(Error::from(ErrorKind::TimedOut))
    } else {
        None
    }
}

fn abort_with(k: &mut Kernel, fd: Fd, reason: AbortReason) {
    let st = k.lookup_mut(fd).unwrap();
    if let Some(tcb) = st.tcb.as_mut() {
        tcb.state = TcpState::Closed;
        match reason {
            AbortReason::Reset => tcb.reset = true,
            AbortReason::TimedOut => tcb.timed_out = true,
        }
        tcb.send_buf.clear();
        tcb.recv_buf.clear();
    }
    if let Some(w) = st.connect_waker.take() {
        w.wake();
    }
    st.wake_read();
    st.wake_write();
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
/// handshaking (`SynReceived`). Charged against the listener's backlog
/// alongside the accept-ready queue.
fn count_children(k: &Kernel, listener_fd: Fd, local: SocketAddr) -> usize {
    k.sockets
        .connections_on(local)
        .filter(|(_, fd)| {
            if *fd == listener_fd {
                return false;
            }
            k.sockets
                .get(*fd)
                .and_then(|s| s.tcb.as_ref())
                .map(|t| t.state == TcpState::SynReceived)
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
/// capacity. Returns `Pending` (parking write-side waker) when the buffer
/// is already full. Segmentation and the actual wire emit happen later,
/// in `segment_all` during `egress`.
pub(super) fn poll_send(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &[u8],
) -> Poll<Result<usize>> {
    let send_cap = k.send_buf_cap;
    let st = match k.lookup_mut(fd) {
        Ok(st) => st,
        Err(e) => return Poll::Ready(Err(e)),
    };
    let Some(tcb) = st.tcb.as_ref() else {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    };
    if let Some(e) = abort_error(tcb) {
        return Poll::Ready(Err(e));
    }
    if tcb.wr_closed {
        return Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe)));
    }
    // Accept writes while the connection is open for sending. CloseWait
    // means the peer finished but we can keep sending until the app
    // closes too.
    if !matches!(tcb.state, TcpState::Established | TcpState::CloseWait) {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    }
    let space = send_cap.saturating_sub(tcb.send_buf.len());
    if space == 0 {
        st.register_write_waker(cx.waker());
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

/// Close the write side. Queues a FIN to ride out after any buffered
/// data drains; state advances on FIN-ACK. Idempotent on an already-
/// shut side.
pub(super) fn poll_shutdown_write(
    k: &mut Kernel,
    fd: Fd,
    _cx: &mut Context<'_>,
) -> Poll<Result<()>> {
    let st = match k.lookup_mut(fd) {
        Ok(st) => st,
        Err(e) => return Poll::Ready(Err(e)),
    };
    let Some(tcb) = st.tcb.as_mut() else {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    };
    if let Some(e) = abort_error(tcb) {
        return Poll::Ready(Err(e));
    }
    if tcb.wr_closed {
        return Poll::Ready(Ok(()));
    }
    // FIN rides out after any queued bytes — its seq sits right past
    // the last byte the app accepted into send_buf.
    let fin_seq = tcb.snd_una.wrapping_add(tcb.send_buf.len() as u32);
    tcb.fin_seq = Some(fin_seq);
    tcb.wr_closed = true;
    tcb.state = match tcb.state {
        TcpState::Established => TcpState::FinWait1,
        TcpState::CloseWait => TcpState::LastAck,
        other => other,
    };
    Poll::Ready(Ok(()))
}

/// Drain bytes from the socket's `recv_buf` into `buf`. Returns
/// `Pending` (parking read-side waker) when the buffer is empty. An ACK
/// is emitted afterward if the drain opened enough window to be worth
/// advertising — avoids silly-window syndrome on tiny reads.
pub(super) fn poll_recv(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &mut [u8],
) -> Poll<Result<usize>> {
    let recv_cap = k.recv_buf_cap;
    let (n, should_update_window, local, remote) = {
        let st = match k.lookup_mut(fd) {
            Ok(st) => st,
            Err(e) => return Poll::Ready(Err(e)),
        };
        // Inspect without holding a borrow across the mutable ops.
        let (empty, peer_fin, abort_err, readable_state, peer) = match st.tcb.as_ref() {
            None => return Poll::Ready(Err(Error::from(ErrorKind::NotConnected))),
            Some(t) => (
                t.recv_buf.is_empty(),
                t.peer_fin,
                abort_error(t),
                matches!(
                    t.state,
                    TcpState::Established
                        | TcpState::FinWait1
                        | TcpState::FinWait2
                        | TcpState::CloseWait
                ),
                t.peer,
            ),
        };
        // Abort wins over buffered data — callers need the error.
        if let Some(e) = abort_err {
            return Poll::Ready(Err(e));
        }
        if empty {
            // Clean EOF — peer sent FIN and we've drained everything.
            // `Ready(Ok(0))` is the standard EOF signal.
            if peer_fin {
                return Poll::Ready(Ok(0));
            }
            if !readable_state {
                return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
            }
            st.register_read_waker(cx.waker());
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
        let should_update = n >= recv_cap / 2;
        (n, should_update, local, peer)
    };

    if should_update_window {
        let (snd_nxt, rcv_nxt, window) = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            (
                tcb.snd_nxt,
                tcb.rcv_nxt,
                advertised_window(recv_cap, tcb.recv_buf.len()),
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

/// Like [`poll_read`] but leaves `recv_buf` intact. No window update
/// is emitted — we haven't actually freed any buffer space.
pub(super) fn poll_peek(
    k: &mut Kernel,
    fd: Fd,
    cx: &mut Context<'_>,
    buf: &mut [u8],
) -> Poll<Result<usize>> {
    let st = match k.lookup_mut(fd) {
        Ok(st) => st,
        Err(e) => return Poll::Ready(Err(e)),
    };
    let Some(tcb) = st.tcb.as_ref() else {
        return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
    };
    if let Some(e) = abort_error(tcb) {
        return Poll::Ready(Err(e));
    }
    if tcb.recv_buf.is_empty() {
        if tcb.peer_fin {
            return Poll::Ready(Ok(0));
        }
        if !matches!(
            tcb.state,
            TcpState::Established | TcpState::FinWait1 | TcpState::FinWait2 | TcpState::CloseWait
        ) {
            return Poll::Ready(Err(Error::from(ErrorKind::NotConnected)));
        }
        st.register_read_waker(cx.waker());
        return Poll::Pending;
    }
    let n = tcb.recv_buf.len().min(buf.len());
    buf[..n].copy_from_slice(&tcb.recv_buf[..n]);
    Poll::Ready(Ok(n))
}

/// Count-based retransmit sweep. For each TCB with unacked data,
/// increment the "egress passes since ACK progress" counter. Once it
/// crosses `retx_threshold`, rewind `snd_nxt = snd_una` so the next
/// `segment_all` pass re-emits from the oldest unacked byte
/// (go-back-N — see the module-level doc for why we don't do SACK).
/// After `retx_max` attempts, abort with `ConnectionReset`.
///
/// Count-based (not time-based) so the mechanism is harness-agnostic:
/// harnesses with fixed-width ticks translate N ticks to sim time,
/// interleaving harnesses see it as a quantum count. Tests care about
/// "dropped packet → retx eventually happens," not exact timing.
pub(super) fn check_retx(k: &mut Kernel) {
    let threshold = k.retx_threshold;
    let max = k.retx_max;
    let candidates: Vec<Fd> = k
        .sockets
        .iter()
        .filter_map(|(fd, st)| {
            let tcb = st.tcb.as_ref()?;
            // Handshake states have implicit in-flight (the SYN or
            // SYN-ACK). Data states use snd_una != snd_nxt.
            let handshake = matches!(tcb.state, TcpState::SynSent | TcpState::SynReceived);
            let data = matches!(
                tcb.state,
                TcpState::Established
                    | TcpState::CloseWait
                    | TcpState::FinWait1
                    | TcpState::Closing
                    | TcpState::LastAck
            ) && tcb.snd_una != tcb.snd_nxt;
            if handshake || data {
                Some(fd)
            } else {
                None
            }
        })
        .collect();

    let mut abort: Vec<Fd> = Vec::new();
    let mut resend_handshake: Vec<Fd> = Vec::new();
    for fd in candidates {
        let tcb = k.sockets.get_mut(fd).unwrap().tcb.as_mut().unwrap();
        tcb.egress_since_ack += 1;
        if tcb.egress_since_ack < threshold {
            continue;
        }
        if tcb.retx_attempts >= max {
            abort.push(fd);
            continue;
        }
        tcb.retx_attempts += 1;
        tcb.egress_since_ack = 0;
        match tcb.state {
            TcpState::SynSent | TcpState::SynReceived => resend_handshake.push(fd),
            _ => {
                // Rewind to oldest unacked byte. `segment_all` below
                // will re-chop from snd_una; any in-flight tail gets
                // re-sent too (go-back-N) — wasteful but correct
                // without an out-of-order receive queue on the peer
                // side.
                tcb.snd_nxt = tcb.snd_una;
            }
        }
    }
    for fd in resend_handshake {
        emit_handshake(k, fd);
    }
    for fd in abort {
        abort_timed_out(k, fd);
    }
}

/// Re-emit the SYN (client, `SynSent`) or SYN-ACK (server,
/// `SynReceived`) for a handshake that's been stuck. Reuses
/// `snd_una - 1` as the seq — matches the ISN used at initial emit,
/// since `snd_una` was set to `isn + 1` there.
fn emit_handshake(k: &mut Kernel, fd: Fd) {
    let st = k.lookup(fd).expect("retx candidate");
    let tcb = st.tcb.as_ref().expect("handshake state has tcb");
    let local = bound_endpoint(st);
    let remote = tcb.peer;
    let seq = tcb.snd_una.wrapping_sub(1);
    let (ack_flag, ack) = match tcb.state {
        TcpState::SynSent => (false, 0),
        TcpState::SynReceived => (true, tcb.rcv_nxt),
        _ => unreachable!("emit_handshake only called for SynSent/SynReceived"),
    };
    emit(
        k,
        local,
        remote,
        TcpSegment {
            src_port: local.port(),
            dst_port: remote.port(),
            seq,
            ack,
            flags: TcpFlags {
                syn: true,
                ack: ack_flag,
                ..TcpFlags::default()
            },
            window: DEFAULT_WINDOW,
            payload: Bytes::new(),
        },
    );
}

/// Sweep all TCP sockets with transmittable bytes or a pending FIN
/// and chop them into MSS-sized segments bounded by the peer's
/// advertised window. Invoked by `egress` before the outbound drain,
/// so newly-segmented packets ride the same pump that handles UDP and
/// handshake traffic.
pub(super) fn segment_all(k: &mut Kernel) {
    let candidates: Vec<Fd> = k
        .sockets
        .iter()
        .filter(|(_, s)| {
            s.tcb
                .as_ref()
                .map(|t| {
                    let transmittable = matches!(
                        t.state,
                        TcpState::Established
                            | TcpState::CloseWait
                            | TcpState::FinWait1
                            | TcpState::Closing
                            | TcpState::LastAck
                    );
                    let has_data = t.send_buf.len() > (t.snd_nxt.wrapping_sub(t.snd_una)) as usize;
                    let fin_pending = t.fin_seq.map(|fs| t.snd_nxt == fs).unwrap_or(false);
                    transmittable && (has_data || fin_pending)
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
    let recv_cap = k.recv_buf_cap;

    loop {
        let (seq, payload, is_fin) = {
            let tcb = k.lookup_mut(fd).unwrap().tcb.as_mut().unwrap();
            let in_flight = tcb.snd_nxt.wrapping_sub(tcb.snd_una) as usize;
            let unsent = tcb.send_buf.len().saturating_sub(in_flight);
            let wnd_remaining = (tcb.snd_wnd as usize).saturating_sub(in_flight);
            let fin_pending = tcb.fin_seq.map(|fs| tcb.snd_nxt == fs).unwrap_or(false);

            if unsent > 0 && wnd_remaining > 0 {
                let n = unsent.min(mss).min(wnd_remaining);
                let start = in_flight;
                let end = start + n;
                let payload = Bytes::copy_from_slice(&tcb.send_buf[start..end]);
                let seq = tcb.snd_nxt;
                tcb.snd_nxt = tcb.snd_nxt.wrapping_add(n as u32);
                (seq, payload, false)
            } else if fin_pending && wnd_remaining > 0 {
                // Emit the FIN. It occupies one sequence number but
                // carries no payload.
                let seq = tcb.snd_nxt;
                tcb.snd_nxt = tcb.snd_nxt.wrapping_add(1);
                (seq, Bytes::new(), true)
            } else {
                return;
            }
        };
        let remote = k.lookup(fd).unwrap().tcb.as_ref().unwrap().peer;
        let (rcv_nxt, window) = {
            let tcb = k.lookup(fd).unwrap().tcb.as_ref().unwrap();
            (tcb.rcv_nxt, advertised_window(recv_cap, tcb.recv_buf.len()))
        };
        emit(
            k,
            local,
            remote,
            TcpSegment {
                src_port: local.port(),
                dst_port: remote.port(),
                seq,
                ack: rcv_nxt,
                flags: TcpFlags {
                    ack: true,
                    psh: !is_fin && !payload.is_empty(),
                    fin: is_fin,
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

fn advertised_window(recv_buf_cap: usize, recv_buf_len: usize) -> u16 {
    recv_buf_cap
        .saturating_sub(recv_buf_len)
        .min(u16::MAX as usize) as u16
}

const DEFAULT_WINDOW: u16 = 65535;
