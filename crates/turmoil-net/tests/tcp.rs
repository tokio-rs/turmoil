use std::io::ErrorKind;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil_net::fixture::{self, ClientServer};
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};
use turmoil_net::{rule, KernelConfig, Packet, Transport, Verdict};

#[test]
fn tcp_accept_completes_handshake() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7100").await.unwrap();
        let (client, (server, server_peer)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7100"), listener.accept(),).unwrap();

        assert_eq!(client.peer_addr().unwrap().port(), 7100);
        assert!(client.local_addr().unwrap().ip().is_loopback());
        assert_eq!(server_peer, client.local_addr().unwrap());
        assert_eq!(server.peer_addr().unwrap(), client.local_addr().unwrap());
    });
}

#[test]
fn tcp_roundtrip() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7400").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7400"), listener.accept(),).unwrap();

        let mut buf = [0u8; 5];
        tokio::try_join!(client.write_all(b"hello"), server.read_exact(&mut buf)).unwrap();
        assert_eq!(&buf, b"hello");
    });
}

#[test]
fn tcp_read_wakes_on_data() {
    // Parked read registers a waker; a later write must wake it.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7500").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7500"), listener.accept(),).unwrap();

        let reader = tokio::spawn(async move {
            let mut buf = [0u8; 3];
            server.read_exact(&mut buf).await.unwrap();
            buf
        });

        client.write_all(b"hey").await.unwrap();
        assert_eq!(&reader.await.unwrap(), b"hey");
    });
}

#[test]
fn tcp_write_backpressure() {
    // Send buffer cap is 64 KiB; recv buffer cap is 64 KiB. Writing
    // 256 KiB without the server ever reading must block once both
    // are full. Draining the server releases the writer.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7600").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7600"), listener.accept(),).unwrap();

        const N: usize = 256 * 1024;
        let mut writer = tokio::spawn(async move {
            client.write_all(&vec![0u8; N]).await.unwrap();
        });

        // With no one reading, write_all can't complete.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut writer)
                .await
                .is_err(),
            "write_all completed without drain"
        );

        // Drain the server side; writer catches up and finishes.
        let mut buf = vec![0u8; N];
        server.read_exact(&mut buf).await.unwrap();
        writer.await.unwrap();
    });
}

#[test]
fn tcp_segments_respect_mss() {
    // Loopback MTU is 65536; MSS = 65536 - 20 (IP) - 20 (TCP) = 65496.
    // A 200 KiB write must produce multiple segments. We can't observe
    // the wire but we can confirm the byte stream arrives intact.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7700").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7700"), listener.accept(),).unwrap();

        let payload = (0..50_000u32)
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut got = vec![0u8; 200_000];
        tokio::try_join!(client.write_all(&payload), server.read_exact(&mut got),).unwrap();
        assert_eq!(got, payload);
    });
}

#[test]
fn tcp_graceful_close_signals_eof() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7800").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7800"), listener.accept(),).unwrap();

        let mut got = Vec::new();
        tokio::try_join!(
            async {
                client.write_all(b"bye").await?;
                client.shutdown().await
            },
            server.read_to_end(&mut got),
        )
        .unwrap();
        assert_eq!(got, b"bye");
    });
}

#[test]
fn tcp_accept_backlog_drops_excess_syns() {
    // Backlog=2: two handshaked-but-unaccepted connections fill the
    // ready queue. A 3rd connect's SYN is dropped by the cap. With
    // SYN retx, the 3rd eventually lands if a slot opens; if not, it
    // times out. Here we drain a slot first and confirm the new
    // connect succeeds via retx.
    fixture::lo_with_config(KernelConfig::default().default_backlog(2), async {
        let listener = TcpListener::bind("127.0.0.1:8100").await.unwrap();

        let _a = TcpStream::connect("127.0.0.1:8100").await.unwrap();
        let _b = TcpStream::connect("127.0.0.1:8100").await.unwrap();

        // Drain a slot, then a fresh connect fits — possibly via SYN
        // retx if the very first SYN raced the accept.
        let _ = listener.accept().await.unwrap();
        let _c = TcpStream::connect("127.0.0.1:8100").await.unwrap();
    });
}

#[test]
fn tcp_connect_to_closed_port_is_refused() {
    fixture::lo(async {
        let err = match TcpStream::connect("127.0.0.1:9999").await {
            Ok(_) => panic!("connect unexpectedly succeeded"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), ErrorKind::ConnectionRefused);
    });
}

#[test]
fn tcp_half_close_send_then_peer_writes_back() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8200").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8200"), listener.accept(),).unwrap();

        // Client writes, shuts down its write side. Server reads + EOF,
        // then writes back. Client reads despite its write side being
        // closed — that's half-close.
        let mut client_read = Vec::new();
        let mut server_read = Vec::new();
        tokio::try_join!(
            async {
                client.write_all(b"hi").await?;
                client.shutdown().await?;
                client.read_to_end(&mut client_read).await.map(|_| ())
            },
            async {
                server.read_to_end(&mut server_read).await?;
                server.write_all(b"back").await?;
                server.shutdown().await
            },
        )
        .unwrap();
        assert_eq!(server_read, b"hi");
        assert_eq!(client_read, b"back");
    });
}

#[test]
fn tcp_drop_with_unread_bytes_sends_rst() {
    // Server sends bytes to client but client drops without reading.
    // Client's drop emits RST; server observes ConnectionReset on a
    // subsequent write.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8300").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8300"), listener.accept(),).unwrap();

        server.write_all(b"unread").await.unwrap();
        // Wait until the bytes actually reach the client's recv_buf —
        // on_close checks recv_buf to decide RST vs. clean close, so
        // we need them there before drop. peek observes without
        // consuming.
        let mut p = [0u8; 6];
        client.peek(&mut p).await.unwrap();
        drop(client);

        // The first write after the peer's drop may succeed (RST
        // hasn't landed yet); keep writing until one surfaces the
        // reset. Each iteration awaits, so the stepper runs and
        // carries the RST across. Bounded so a regression surfaces
        // as a timeout rather than an infinite loop.
        let observed = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            loop {
                if let Err(e) = server.write_all(b"x").await {
                    break e.kind();
                }
            }
        })
        .await
        .expect("never observed RST");
        assert_eq!(observed, ErrorKind::ConnectionReset);
    });
}

#[test]
fn tcp_inbound_rst_wakes_parked_read() {
    // Park a reader, then have the peer drop with unread bytes on
    // *its* side — that triggers an abortive close (RST), which our
    // parked reader must observe as ConnectionReset.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8400").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8400"), listener.accept(),).unwrap();

        // Server writes to client — those bytes must land in the
        // client's recv_buf before we drop, otherwise on_close sees
        // an empty recv_buf and elects a clean close. peek blocks on
        // the actual state transition.
        server.write_all(b"unread").await.unwrap();
        let mut p = [0u8; 6];
        client.peek(&mut p).await.unwrap();

        // Park server's read, then drop the client. The RST emitted
        // by client's abortive close wakes the parked read.
        let read = tokio::spawn(async move {
            let mut buf = [0u8; 4];
            server.read(&mut buf).await
        });
        drop(client);

        let err = read.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    });
}

#[test]
fn tcp_stream_options_roundtrip() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8600").await.unwrap();
        let (client, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8600"), listener.accept(),).unwrap();

        assert_eq!(client.ttl().unwrap(), 64);
        client.set_ttl(16).unwrap();
        assert_eq!(client.ttl().unwrap(), 16);

        assert!(!client.nodelay().unwrap());
        client.set_nodelay(true).unwrap();
        assert!(client.nodelay().unwrap());
    });
}

#[test]
fn tcp_try_read_would_block_on_empty() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8700").await.unwrap();
        let (client, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8700"), listener.accept(),).unwrap();

        let mut buf = [0u8; 8];
        assert_eq!(
            client.try_read(&mut buf).unwrap_err().kind(),
            ErrorKind::WouldBlock
        );
    });
}

#[test]
fn tcp_peek_leaves_data_for_read() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:8800").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:8800"), listener.accept(),).unwrap();

        client.write_all(b"PING").await.unwrap();

        let mut p = [0u8; 4];
        let n = server.peek(&mut p).await.unwrap();
        assert_eq!(&p[..n], b"PING");

        // Same bytes still there for a real read.
        let mut r = [0u8; 4];
        server.read_exact(&mut r).await.unwrap();
        assert_eq!(&r, b"PING");
    });
}

#[test]
fn tcp_listener_ttl_roundtrips() {
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7300").await.unwrap();
        assert_eq!(listener.ttl().unwrap(), 64);
        listener.set_ttl(16).unwrap();
        assert_eq!(listener.ttl().unwrap(), 16);
        assert_eq!(
            listener.set_ttl(256).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    });
}

#[test]
fn tcp_split_concurrent_read_write() {
    // `tokio::io::split` holds a read half and a write half that each
    // park on the socket's wakers independently. If read/write shared
    // a single-slot waker we'd lose one side's wake when the other
    // side registers. Reading parks first; writing unparks after the
    // peer sends some bytes; final read sees the reply.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7400").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7400"), listener.accept()).unwrap();
        let (mut rd, mut wr) = tokio::io::split(client);

        let mut buf = [0u8; 5];
        tokio::try_join!(
            async {
                wr.write_all(b"hello").await?;
                let mut reply = [0u8; 3];
                rd.read_exact(&mut reply).await?;
                assert_eq!(&reply, b"hi!");
                Ok::<_, std::io::Error>(())
            },
            async {
                server.read_exact(&mut buf).await?;
                assert_eq!(&buf, b"hello");
                server.write_all(b"hi!").await?;
                Ok(())
            },
        )
        .unwrap();
    });
}

#[test]
fn tcp_borrowed_split() {
    // TcpStream::split returns borrowed halves that must operate
    // concurrently within the same task/scope.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7401").await.unwrap();
        let (mut client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7401"), listener.accept()).unwrap();
        let (mut rd, mut wr) = client.split();

        let mut reply = [0u8; 3];
        tokio::try_join!(
            async {
                wr.write_all(b"ping").await?;
                rd.read_exact(&mut reply).await?;
                Ok::<_, std::io::Error>(())
            },
            async {
                let mut buf = [0u8; 4];
                server.read_exact(&mut buf).await?;
                server.write_all(b"png").await?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(&reply, b"png");
    });
}

#[test]
fn tcp_owned_split_and_reunite() {
    // into_split vends owned halves; reunite puts them back together.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7402").await.unwrap();
        let (client, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7402"), listener.accept()).unwrap();
        let (rd, wr) = client.into_split();

        // Reunite from either side.
        let stream = rd.reunite(wr).expect("same origin");
        assert_eq!(stream.peer_addr().unwrap().port(), 7402);
    });
}

#[test]
fn tcp_owned_split_reunite_mismatch_errors() {
    // Halves from different streams must not reunite.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7403").await.unwrap();
        let (a, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7403"), listener.accept()).unwrap();
        let (b, _) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7403"), listener.accept()).unwrap();

        let (rd_a, _wr_a) = a.into_split();
        let (_rd_b, wr_b) = b.into_split();

        let err = rd_a.reunite(wr_b).unwrap_err();
        assert!(err.to_string().contains("not from the same socket"));
    });
}

#[test]
fn tcp_owned_write_half_drop_shuts_down() {
    // Dropping OwnedWriteHalf must shutdown write side — peer sees EOF.
    fixture::lo(async {
        let listener = TcpListener::bind("127.0.0.1:7404").await.unwrap();
        let (client, (mut server, _)) =
            tokio::try_join!(TcpStream::connect("127.0.0.1:7404"), listener.accept()).unwrap();
        let (_rd, wr) = client.into_split();
        drop(wr);

        let mut buf = [0u8; 1];
        let n = server.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "dropped write half should send FIN → server sees EOF");
    });
}

/// Drop the first N TCP data segments (payload-bearing, non-handshake)
/// observed on the fabric. SYN/SYN-ACK/pure-ACK/FIN traffic is never
/// dropped — this isolates data-path retx from handshake flows.
fn drop_first_n_data_segments(n: usize) -> impl FnMut(&Packet) -> Verdict {
    let mut dropped = 0usize;
    move |pkt: &Packet| match &pkt.payload {
        Transport::Tcp(s) if !s.payload.is_empty() && dropped < n => {
            dropped += 1;
            Verdict::Drop
        }
        _ => Verdict::Pass,
    }
}

#[test]
fn tcp_retx_recovers_from_single_segment_drop() {
    // Server writes, client reads. The rule drops the first data
    // segment regardless of direction; retx fills the gap. Client's
    // read_exact is the natural join point.
    ClientServer::new()
        .server("server", async move {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(b"hello").await.unwrap();
        })
        .run("client", async move {
            rule(drop_first_n_data_segments(1)).forget();
            let mut c = TcpStream::connect("server:9000").await.unwrap();
            let mut buf = [0u8; 5];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
        });
}

#[test]
fn tcp_retx_go_back_n_recovers_multi_segment_write() {
    // Configure a tiny MTU so a modest write splits into many
    // segments, then drop just the first. Go-back-N resends the whole
    // window from snd_una; the receiver only accepts in-order, so
    // segments 2..N land on-time and the retx fills the gap.
    const N: usize = 8 * 1024;
    let payload: Vec<u8> = (0..N).map(|i| (i % 251) as u8).collect();
    let payload_for_server = payload.clone();
    let cfg = KernelConfig::default().mtu(600).loopback_mtu(600);
    ClientServer::with_config(cfg)
        .server("server", async move {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (mut sock, _) = listener.accept().await.unwrap();
            sock.write_all(&payload_for_server).await.unwrap();
        })
        .run("client", async move {
            rule(drop_first_n_data_segments(1)).forget();
            let mut c = TcpStream::connect("server:9000").await.unwrap();
            let mut buf = vec![0u8; N];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, payload);
        });
}

#[test]
fn tcp_retx_exhaustion_times_out() {
    // Drop every data segment ever seen. The sender retries up to
    // retx_max times, then aborts with TimedOut (matching Linux's
    // ETIMEDOUT from tcp_retries2 exhaustion — distinct from
    // ConnectionReset, which only fires on an actual inbound RST).
    // Bounded by a timeout so a regression surfaces as a hang rather
    // than an infinite loop.
    ClientServer::new()
        .server("server", async move {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (_sock, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|pkt: &Packet| match &pkt.payload {
                Transport::Tcp(s) if !s.payload.is_empty() => Verdict::Drop,
                _ => Verdict::Pass,
            })
            .forget();

            let mut c = TcpStream::connect("server:9000").await.unwrap();
            // write_all may succeed into the send buffer before retx
            // exhaustion flips the state, so loop until it surfaces.
            let kind = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let Err(e) = c.write_all(b"x").await {
                        break e.kind();
                    }
                }
            })
            .await
            .expect("connection never aborted");
            assert_eq!(kind, ErrorKind::TimedOut);
        });
}

#[test]
fn tcp_syn_retx_recovers_from_single_drop() {
    // Drop the first SYN. Retransmit path re-emits it and the
    // handshake completes on the second attempt.
    ClientServer::new()
        .server("server", async move {
            let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            let (_sock, _) = listener.accept().await.unwrap();
        })
        .run("client", async move {
            let mut dropped = false;
            rule(move |pkt: &Packet| match &pkt.payload {
                Transport::Tcp(s) if s.flags.syn && !s.flags.ack && !dropped => {
                    dropped = true;
                    Verdict::Drop
                }
                _ => Verdict::Pass,
            })
            .forget();
            let _c = TcpStream::connect("server:9000").await.unwrap();
        });
}

#[test]
fn tcp_syn_retx_exhaustion_times_out() {
    // Drop every SYN. Connect retries up to retx_max times then
    // surfaces TimedOut (Linux ETIMEDOUT from tcp_syn_retries
    // exhaustion). Distinct from ConnectionRefused, which is what
    // the peer RST'ing a SYN would produce.
    ClientServer::new()
        .server("server", async move {
            let _l = TcpListener::bind("0.0.0.0:9000").await.unwrap();
            std::future::pending::<()>().await;
        })
        .run("client", async move {
            rule(|pkt: &Packet| match &pkt.payload {
                Transport::Tcp(s) if s.flags.syn && !s.flags.ack => Verdict::Drop,
                _ => Verdict::Pass,
            })
            .forget();
            let err = TcpStream::connect("server:9000").await.unwrap_err();
            assert_eq!(err.kind(), ErrorKind::TimedOut);
        });
}
