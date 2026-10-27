//! Manual release-mode scaling benchmark; timings are not a CI assertion.
//!
//! cargo test -p turmoil-net --release udp_delivery_scaling -- --ignored --nocapture

use crate::kernel::{Addr, Kernel, Packet, SocketOption, Transport, Type, UdpDatagram};
use bytes::Bytes;
use std::hint::black_box;
use std::net::SocketAddr;
use std::time::Instant;

#[test]
#[ignore = "manual release-mode performance benchmark"]
fn udp_delivery_scaling() {
    for mode in ["unicast", "multicast", "broadcast"] {
        for idle in [0, 128, 1024, 4096] {
            let mut k = Kernel::new();
            k.add_address("10.0.0.1".parse().unwrap());
            for i in 0..idle {
                k.bind(
                    &Addr::Inet(SocketAddr::from(([0, 0, 0, 0], 10000 + i))),
                    Type::Dgram,
                )
                .unwrap();
            }
            let fd = k
                .bind(&Addr::Inet("0.0.0.0:9000".parse().unwrap()), Type::Dgram)
                .unwrap();
            if mode == "multicast" {
                k.set_option(
                    fd,
                    SocketOption::IpAddMembership {
                        group: "239.10.10.10".parse().unwrap(),
                        iface: "0.0.0.0".parse().unwrap(),
                    },
                )
                .unwrap();
            }
            let dst = match mode {
                "multicast" => "239.10.10.10",
                "broadcast" => "255.255.255.255",
                _ => "10.0.0.1",
            };
            let pkt = Packet {
                src: "10.0.0.2".parse().unwrap(),
                dst: dst.parse().unwrap(),
                ttl: 1,
                payload: Transport::Udp(UdpDatagram {
                    src_port: 8000,
                    dst_port: 9000,
                    payload: Bytes::from_static(&[0; 64]),
                }),
            };
            for sample in 0..5 {
                let count = 50000;
                let start = Instant::now();
                for _ in 0..count {
                    k.deliver(black_box(pkt.clone()));
                    let q = &mut k.lookup_mut(fd).unwrap().recv_queue;
                    assert_eq!(q.len(), 1);
                    black_box(q.pop_front());
                }
                println!(
                    "PERF,{mode},{idle},{sample},{:.2}",
                    start.elapsed().as_nanos() as f64 / count as f64
                );
            }
        }
    }
}
