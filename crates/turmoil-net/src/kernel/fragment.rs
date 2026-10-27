//! UDP IP fragmentation and bounded per-interface reassembly.

use std::collections::BTreeMap;
use std::io;
use std::net::IpAddr;

use bytes::{BufMut, Bytes, BytesMut};
use indexmap::IndexMap;

use super::{InterfaceId, IpFragment, Packet, Transport, UdpDatagram, EMSGSIZE};

type Key = (InterfaceId, IpAddr, IpAddr, u32);

/// Fragment sizes include the UDP header in the first fragment, and
/// the IPv6 Fragment extension header on every IPv6 fragment.
pub(super) fn split(packet: Packet, mtu: u32, id: u32) -> io::Result<Vec<Packet>> {
    let ip_header = if packet.dst.is_ipv4() { 20 } else { 48 };
    let chunk = (mtu.saturating_sub(ip_header) as usize) & !7;
    if chunk < 8 {
        return Err(io::Error::from_raw_os_error(EMSGSIZE));
    }
    let Transport::Udp(datagram) = packet.payload else {
        unreachable!("only UDP needs IP fragmentation");
    };
    let mut payload = BytesMut::with_capacity(8 + datagram.payload.len());
    payload.put_u16(datagram.src_port);
    payload.put_u16(datagram.dst_port);
    payload.put_u16((8 + datagram.payload.len()) as u16);
    payload.put_u16(0); // Checksums are outside this packet model.
    payload.extend_from_slice(&datagram.payload);
    let payload = payload.freeze();
    let mut fragments = Vec::with_capacity(payload.len().div_ceil(chunk));
    for offset in (0..payload.len()).step_by(chunk) {
        let end = (offset + chunk).min(payload.len());
        fragments.push(Packet {
            src: packet.src,
            dst: packet.dst,
            ttl: packet.ttl,
            payload: Transport::Fragment(IpFragment {
                id,
                offset: offset as u32,
                more: end < payload.len(),
                payload: payload.slice(offset..end),
            }),
        });
    }
    Ok(fragments)
}

#[derive(Debug)]
struct Assembly {
    pieces: BTreeMap<u32, Bytes>,
    end: Option<u32>,
    bytes: usize,
    expires: u64,
    invalid: bool,
}

#[derive(Debug)]
pub(super) struct Reassembly {
    entries: IndexMap<Key, Assembly>,
    buffered: usize,
    timeout: u32,
    capacity: usize,
    queue_capacity: usize,
}

impl Reassembly {
    pub fn new(timeout: u32, capacity: usize, queue_capacity: usize) -> Self {
        Self {
            entries: IndexMap::new(),
            buffered: 0,
            timeout,
            capacity,
            queue_capacity,
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.buffered = 0;
    }

    pub fn expire(&mut self, tick: u64) {
        self.entries.retain(|_, assembly| {
            if assembly.expires <= tick {
                self.buffered -= assembly.bytes;
                false
            } else {
                true
            }
        });
    }

    pub fn receive(
        &mut self,
        packet: &Packet,
        fragment: &IpFragment,
        scope: InterfaceId,
        tick: u64,
    ) -> Option<Packet> {
        let len = u32::try_from(fragment.payload.len()).ok()?;
        let end = fragment.offset.checked_add(len)?;
        let limit = if packet.dst.is_ipv4() { 65515 } else { 65535 };
        if len == 0
            || !fragment.offset.is_multiple_of(8)
            || (fragment.more && !len.is_multiple_of(8))
            || end > limit
            || (fragment.offset == 0 && len < 8)
        {
            return None;
        }
        let key = (scope, packet.src, packet.dst, fragment.id);
        if !self.entries.contains_key(&key) && self.entries.len() >= self.queue_capacity {
            return None;
        }
        // IPv4 reassembly defaults to 30 seconds; IPv6 to 60. The
        // harness translates this timeout into its egress quantum.
        let lifetime = u64::from(self.timeout) * if packet.dst.is_ipv4() { 1 } else { 2 };
        let assembly = self.entries.entry(key).or_insert_with(|| Assembly {
            pieces: BTreeMap::new(),
            end: None,
            bytes: 0,
            expires: tick.saturating_add(lifetime),
            invalid: false,
        });
        if assembly.invalid {
            return None;
        }

        let inconsistent_end = assembly.end.is_some_and(|total| {
            end > total || (!fragment.more && end != total) || (fragment.more && end >= total)
        }) || (!fragment.more
            && assembly
                .pieces
                .iter()
                .any(|(&start, data)| start + data.len() as u32 > end))
            || (!fragment.more
                && assembly.end != Some(end)
                && assembly.pieces.contains_key(&fragment.offset));
        let same = assembly.pieces.get(&fragment.offset);
        if !inconsistent_end && same.is_some_and(|data| data == &fragment.payload) {
            return None; // Harmless duplicate of exactly the same fragment.
        }
        let overlap = assembly
            .pieces
            .range(..=fragment.offset)
            .next_back()
            .is_some_and(|(&start, data)| start + data.len() as u32 > fragment.offset)
            || assembly
                .pieces
                .range(fragment.offset..)
                .next()
                .is_some_and(|(&start, _)| start < end);
        if inconsistent_end || overlap {
            // Reject the entire datagram on conflicting overlap. Keep
            // its key until expiry so a later tail cannot revive it.
            self.buffered -= assembly.bytes;
            assembly.bytes = 0;
            assembly.pieces.clear();
            assembly.invalid = true;
            return None;
        }
        if self.buffered.saturating_add(len as usize) > self.capacity {
            return None;
        }
        if !fragment.more {
            assembly.end = Some(end);
        }
        // A slice can retain the entire outbound datagram allocation;
        // own only these bytes so the memory bound is meaningful.
        assembly
            .pieces
            .insert(fragment.offset, Bytes::copy_from_slice(&fragment.payload));
        assembly.bytes += len as usize;
        self.buffered += len as usize;
        if assembly.end != Some(assembly.bytes as u32) {
            return None;
        }

        let assembly = self.entries.shift_remove(&key).unwrap();
        self.buffered -= assembly.bytes;
        let mut payload = BytesMut::with_capacity(assembly.bytes);
        for piece in assembly.pieces.values() {
            payload.extend_from_slice(piece);
        }
        if payload.len() < 8
            || u16::from_be_bytes([payload[4], payload[5]]) as usize != payload.len()
        {
            return None;
        }
        Some(Packet {
            src: packet.src,
            dst: packet.dst,
            ttl: packet.ttl,
            payload: Transport::Udp(UdpDatagram {
                src_port: u16::from_be_bytes([payload[0], payload[1]]),
                dst_port: u16::from_be_bytes([payload[2], payload[3]]),
                payload: payload.freeze().slice(8..),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn datagram(v6: bool) -> Packet {
        Packet {
            src: if v6 { "fd00::1" } else { "10.0.0.1" }.parse().unwrap(),
            dst: if v6 { "fd00::2" } else { "10.0.0.2" }.parse().unwrap(),
            ttl: 64,
            payload: Transport::Udp(UdpDatagram {
                src_port: 1234,
                dst_port: 5678,
                payload: Bytes::from((0..4096).map(|i| i as u8).collect::<Vec<_>>()),
            }),
        }
    }

    fn receive(r: &mut Reassembly, packet: &Packet, tick: u64) -> Option<Packet> {
        let Transport::Fragment(f) = &packet.payload else {
            panic!("fragment")
        };
        r.receive(packet, f, InterfaceId::Link, tick)
    }

    #[test]
    fn reordered_and_duplicate_fragments_reassemble_in_both_families() {
        for v6 in [false, true] {
            let original = datagram(v6);
            let fragments = split(original.clone(), 1500, 7).unwrap();
            assert_eq!(fragments.len(), 3);
            for (index, packet) in fragments.iter().enumerate() {
                assert!(packet.size() <= 1500);
                let Transport::Fragment(f) = &packet.payload else {
                    panic!("fragment")
                };
                assert!(f.offset.is_multiple_of(8));
                assert_eq!(f.more, index < fragments.len() - 1);
                assert_eq!(f.ports(), (index == 0).then_some((1234, 5678)));
            }
            let mut r = Reassembly::new(30, 8192, 4);
            assert!(receive(&mut r, &fragments[2], 0).is_none());
            assert!(receive(&mut r, &fragments[2], 0).is_none());
            assert!(receive(&mut r, &fragments[1], 0).is_none());
            assert_eq!(receive(&mut r, &fragments[0], 0), Some(original));
            assert_eq!(r.buffered, 0);
            assert!(r.entries.is_empty());
        }
    }

    #[test]
    fn missing_fragment_expires_without_partial_delivery() {
        for v6 in [false, true] {
            let fragments = split(datagram(v6), 1500, 7).unwrap();
            let mut r = Reassembly::new(30, 8192, 4);
            assert!(receive(&mut r, &fragments[0], 0).is_none());
            assert!(receive(&mut r, &fragments[2], 20).is_none());
            let expiry = if v6 { 60 } else { 30 };
            r.expire(expiry - 1);
            assert_eq!(r.entries.len(), 1);
            r.expire(expiry);
            assert_eq!(r.buffered, 0);
            assert!(r.entries.is_empty());
            assert!(receive(&mut r, &fragments[1], expiry).is_none());
        }
    }

    #[test]
    fn overlapping_fragments_reject_entire_datagram_until_expiry() {
        for v6 in [false, true] {
            let fragments = split(datagram(v6), 1500, 7).unwrap();
            let mut overlap = fragments[1].clone();
            let Transport::Fragment(f) = &mut overlap.payload else {
                panic!("fragment")
            };
            f.offset = 8;
            let mut r = Reassembly::new(30, 8192, 4);
            assert!(receive(&mut r, &fragments[0], 0).is_none());
            assert!(receive(&mut r, &overlap, 0).is_none());
            for packet in &fragments {
                assert!(receive(&mut r, packet, 0).is_none());
            }
            assert_eq!(r.buffered, 0);
            r.expire(60);
            let mut result = None;
            for packet in &fragments {
                result = receive(&mut r, packet, 60);
            }
            assert_eq!(result, Some(datagram(v6)));
        }
    }

    #[test]
    fn incomplete_reassembly_has_byte_and_queue_bounds() {
        let fragments = split(datagram(false), 1500, 7).unwrap();
        let mut r = Reassembly::new(30, 1500, 1);
        assert!(receive(&mut r, &fragments[0], 0).is_none());
        let buffered = r.buffered;
        assert!(receive(&mut r, &fragments[1], 0).is_none());
        assert_eq!(r.buffered, buffered);
        let second = split(datagram(false), 1500, 8).unwrap();
        assert!(receive(&mut r, &second[0], 0).is_none());
        assert_eq!(r.entries.len(), 1);
        r.clear();
        assert_eq!(r.buffered, 0);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn reassembly_is_isolated_by_source_and_interface() {
        let a = datagram(false);
        let mut b = a.clone();
        b.src = "10.0.0.3".parse().unwrap();
        let a_fragments = split(a.clone(), 1500, 7).unwrap();
        let b_fragments = split(b.clone(), 1500, 7).unwrap();
        let mut r = Reassembly::new(30, 32768, 4);
        for i in 0..2 {
            assert!(receive(&mut r, &a_fragments[i], 0).is_none());
            assert!(receive(&mut r, &b_fragments[i], 0).is_none());
        }
        let Transport::Fragment(last) = &a_fragments[2].payload else {
            panic!("fragment")
        };
        assert!(r
            .receive(&a_fragments[2], last, InterfaceId::Loopback, 0)
            .is_none());
        assert_eq!(receive(&mut r, &a_fragments[2], 0), Some(a));
        assert_eq!(receive(&mut r, &b_fragments[2], 0), Some(b));
    }
}
