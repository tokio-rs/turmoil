//! Per-host socket inspection, Linux `netstat`-style.
//!
//! [`netstat`] returns a snapshot of one host's socket table as a
//! plain [`Netstat`] struct. The [`Display`] impl renders the same
//! columns Linux does, so tests can `println!("{}", netstat("h1"))`
//! and eyeball connection state during a failure.
//!
//! Snapshot — the entries are a copy of socket state at call time.
//! Poking at the returned struct later won't reflect ongoing
//! activity.
//!
//! [`Display`]: std::fmt::Display

use std::fmt::{self, Display};
use std::net::SocketAddr;

use crate::kernel::{Kernel, ListenState, Socket, Tcb, TcpState, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Netstat {
    pub entries: Vec<NetstatEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetstatEntry {
    pub proto: Proto,
    pub recv_q: usize,
    pub send_q: usize,
    pub local: SocketAddr,
    /// `None` renders as `*:*` — an unbound peer (listener, plain UDP).
    pub peer: Option<SocketAddr>,
    /// `None` for UDP sockets without a TCB.
    pub state: Option<NetstatState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

/// Renderable TCP state. Distinct from [`TcpState`] because real
/// netstat shows `LISTEN` as a state, but our kernel models listeners
/// on [`ListenState`] rather than the TCB enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetstatState {
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    Closing,
    /// Terminal. `netstat` filters these entries out — the socket is a
    /// tick away from being reaped, and real Linux would show
    /// `TIME_WAIT` (which our kernel doesn't model). Kept in the enum
    /// so [`From<TcpState>`](From) stays total.
    Closed,
}

impl From<TcpState> for NetstatState {
    fn from(s: TcpState) -> Self {
        match s {
            TcpState::SynSent => NetstatState::SynSent,
            TcpState::SynReceived => NetstatState::SynReceived,
            TcpState::Established => NetstatState::Established,
            TcpState::FinWait1 => NetstatState::FinWait1,
            TcpState::FinWait2 => NetstatState::FinWait2,
            TcpState::CloseWait => NetstatState::CloseWait,
            TcpState::LastAck => NetstatState::LastAck,
            TcpState::Closing => NetstatState::Closing,
            TcpState::Closed => NetstatState::Closed,
        }
    }
}

pub fn snapshot(kernel: &Kernel) -> Netstat {
    let mut entries = Vec::new();
    for (_fd, sock) in kernel.sockets() {
        if let Some(entry) = entry_for(sock) {
            entries.push(entry);
        }
    }
    Netstat { entries }
}

fn entry_for(sock: &Socket) -> Option<NetstatEntry> {
    let bound = sock.bound.as_ref()?;
    let local = SocketAddr::new(bound.local_addr, bound.local_port);

    match sock.ty {
        Type::Stream => tcp_entry(sock, local),
        Type::Dgram => Some(udp_entry(sock, local)),
        Type::SeqPacket => None,
    }
}

fn tcp_entry(sock: &Socket, local: SocketAddr) -> Option<NetstatEntry> {
    if let Some(tcb) = sock.tcb.as_ref() {
        // `Closed` is a transient pre-reap state — the socket is one
        // egress pass away from leaving the table. Real Linux would
        // show TIME_WAIT here, but our kernel doesn't model it (no
        // 2×MSL timer, no bind-conflict hold), so rendering either
        // would be a lie. Hide it instead.
        if tcb.state == TcpState::Closed {
            return None;
        }
        return Some(tcb_entry(tcb, local));
    }
    if let Some(listen) = sock.listen.as_ref() {
        return Some(listen_entry(listen, local));
    }
    None
}

fn tcb_entry(tcb: &Tcb, local: SocketAddr) -> NetstatEntry {
    // send_buf holds `[in-flight | queued]` — both count as Send-Q in
    // real netstat, which reports unACK'd + unsent together.
    let send_q = tcb.send_buf.len();
    let recv_q = tcb.recv_buf.len();
    NetstatEntry {
        proto: Proto::Tcp,
        recv_q,
        send_q,
        local,
        peer: Some(tcb.peer),
        state: Some(NetstatState::from(tcb.state)),
    }
}

fn listen_entry(listen: &ListenState, local: SocketAddr) -> NetstatEntry {
    // Linux convention for LISTEN: Recv-Q is the current accept-queue
    // depth (completed handshakes waiting on accept()), Send-Q is the
    // configured backlog. Half-open (SYN_RCVD) connections aren't
    // counted here — they render as their own rows.
    NetstatEntry {
        proto: Proto::Tcp,
        recv_q: listen.ready.len(),
        send_q: listen.backlog,
        local,
        peer: None,
        state: Some(NetstatState::Listen),
    }
}

fn udp_entry(sock: &Socket, local: SocketAddr) -> NetstatEntry {
    let recv_q: usize = sock.recv_queue.iter().map(|(_, b)| b.len()).sum();
    NetstatEntry {
        proto: Proto::Udp,
        recv_q,
        send_q: 0,
        local,
        peer: None,
        state: None,
    }
}

impl Display for Proto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        })
    }
}

impl Display for NetstatState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NetstatState::Listen => "LISTEN",
            NetstatState::SynSent => "SYN_SENT",
            NetstatState::SynReceived => "SYN_RCVD",
            NetstatState::Established => "ESTABLISHED",
            NetstatState::FinWait1 => "FIN_WAIT1",
            NetstatState::FinWait2 => "FIN_WAIT2",
            NetstatState::CloseWait => "CLOSE_WAIT",
            NetstatState::LastAck => "LAST_ACK",
            NetstatState::Closing => "CLOSING",
            NetstatState::Closed => "CLOSED",
        })
    }
}

impl Display for Netstat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Linux netstat column order: Proto, Recv-Q, Send-Q, Local
        // Address, Foreign Address, State.
        const HEADERS: [&str; 6] = [
            "Proto",
            "Recv-Q",
            "Send-Q",
            "Local Address",
            "Foreign Address",
            "State",
        ];

        let rows: Vec<[String; 6]> = self
            .entries
            .iter()
            .map(|e| {
                [
                    e.proto.to_string(),
                    e.recv_q.to_string(),
                    e.send_q.to_string(),
                    e.local.to_string(),
                    e.peer
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "*:*".into()),
                    e.state.map(|s| s.to_string()).unwrap_or_default(),
                ]
            })
            .collect();

        let mut widths = HEADERS.map(str::len);
        for row in &rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.len());
            }
        }

        write_row(f, &HEADERS.map(String::from), &widths)?;
        for row in &rows {
            write_row(f, row, &widths)?;
        }
        Ok(())
    }
}

fn write_row(f: &mut fmt::Formatter<'_>, row: &[String; 6], widths: &[usize; 6]) -> fmt::Result {
    // Recv-Q / Send-Q are right-aligned (numeric columns in Linux
    // netstat); everything else is left-aligned. The last column gets
    // no trailing padding.
    for (i, cell) in row.iter().enumerate() {
        if i > 0 {
            f.write_str(" ")?;
        }
        let w = widths[i];
        let right_align = matches!(i, 1 | 2);
        if i == row.len() - 1 {
            f.write_str(cell)?;
        } else if right_align {
            write!(f, "{cell:>w$}")?;
        } else {
            write!(f, "{cell:<w$}")?;
        }
    }
    writeln!(f)
}
