//! `ready`, `readable`, `writable`, and the `poll_*_ready` family.
//!
//! Streams use one-byte buffers so a two-byte write fills both: the
//! first byte waits unread at the peer, the second in our send buffer
//! behind a zero window.

use std::future::{poll_fn, Future};
use std::io::{self, ErrorKind, IoSliceMut};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, Interest, ReadBuf};
use tokio::time::{sleep, timeout};
use tokio_test::{assert_pending, assert_ready, assert_ready_ok, task};
use turmoil_net::fixture;
use turmoil_net::shim::tokio::net::{lookup_host, TcpListener, TcpStream, UdpSocket};
use turmoil_net::KernelConfig;

fn tiny() -> KernelConfig {
    KernelConfig::default().send_buf_cap(1).recv_buf_cap(1)
}

async fn pair(port: u16) -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let (stream, (peer, _)) =
        tokio::try_join!(TcpStream::connect(("127.0.0.1", port)), listener.accept())?;
    Ok((stream, peer))
}

/// Leave `stream` with nothing to read and no room to write.
async fn fill(stream: &mut TcpStream) {
    stream.write_all(b"xy").await.unwrap();
    sleep(Duration::from_millis(10)).await;
}

#[derive(Clone, Copy)]
enum Wait {
    Readable,
    Writable,
    ReadyRead,
    ReadyWrite,
    ReadyBoth,
    Peek,
}

impl Wait {
    fn write_only(self) -> bool {
        matches!(self, Self::Writable | Self::ReadyWrite)
    }

    async fn run(self, stream: &TcpStream) -> io::Result<()> {
        match self {
            Self::Readable => stream.readable().await,
            Self::Writable => stream.writable().await,
            Self::ReadyRead => stream.ready(Interest::READABLE).await.map(|_| ()),
            Self::ReadyWrite => stream.ready(Interest::WRITABLE).await.map(|_| ()),
            Self::ReadyBoth => stream
                .ready(Interest::READABLE | Interest::WRITABLE)
                .await
                .map(|_| ()),
            Self::Peek => stream.peek(&mut [0; 1]).await.map(|_| ()),
        }
    }
}

const WAITS: [Wait; 6] = [
    Wait::Readable,
    Wait::Writable,
    Wait::ReadyRead,
    Wait::ReadyWrite,
    Wait::ReadyBoth,
    Wait::Peek,
];

/// Make `method` ready on `stream` by acting on `peer`.
async fn unblock(method: Wait, peer: &mut TcpStream) {
    if method.write_only() {
        assert_eq!(peer.read_u8().await.unwrap(), b'x');
    } else {
        peer.write_all(b"z").await.unwrap();
    }
    sleep(Duration::from_millis(10)).await;
}

#[test]
fn ready_reports_only_requested_interest() {
    fixture::lo(async {
        let (stream, mut peer) = pair(9100).await.unwrap();
        let ready = stream.ready(Interest::WRITABLE).await.unwrap();
        assert!(ready.is_writable());
        assert!(!ready.is_readable());

        peer.write_all(b"a").await.unwrap();
        let ready = stream.ready(Interest::READABLE).await.unwrap();
        assert!(ready.is_readable());
        assert!(!ready.is_read_closed());
        assert!(!ready.is_writable());
    });
}

#[test]
fn ready_reports_fin_behind_unread_data() {
    fixture::lo(async {
        let (mut stream, mut peer) = pair(9101).await.unwrap();
        peer.write_all(&[42; 64]).await.unwrap();
        peer.shutdown().await.unwrap();
        sleep(Duration::from_millis(10)).await;

        let ready = stream.ready(Interest::READABLE).await.unwrap();
        assert!(ready.is_readable());
        assert!(ready.is_read_closed());

        // Readiness consumed nothing.
        let mut received = Vec::new();
        stream.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, vec![42; 64]);

        // Only one direction is shut, so writes are still open.
        let ready = stream
            .ready(Interest::READABLE | Interest::WRITABLE)
            .await
            .unwrap();
        assert!(ready.is_read_closed());
        assert!(!ready.is_write_closed());
    });
}

#[test]
fn reset_makes_every_direction_ready() {
    fixture::lo(async {
        let (mut stream, peer) = pair(9102).await.unwrap();
        stream.write_all(b"unread").await.unwrap();
        sleep(Duration::from_millis(10)).await;
        drop(peer); // Unread data → RST.

        let ready = stream.ready(Interest::READABLE).await.unwrap();
        assert!(ready.is_read_closed());
        assert!(!ready.is_write_closed());

        let ready = stream
            .ready(Interest::READABLE | Interest::WRITABLE | Interest::ERROR)
            .await
            .unwrap();
        assert!(ready.is_read_closed());
        assert!(ready.is_write_closed());
        assert!(ready.is_error());

        // As in tokio, readiness itself succeeds; the I/O reports.
        stream.writable().await.unwrap();
        stream.readable().await.unwrap();
        let err = stream.try_write(b"x").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ConnectionReset);
    });
}

#[test]
fn readable_then_try_read_reports_eof() {
    fixture::lo(async {
        let (stream, mut peer) = pair(9103).await.unwrap();
        assert!(timeout(Duration::from_secs(1), stream.readable())
            .await
            .is_err());

        peer.write_all(b"ab").await.unwrap();
        peer.shutdown().await.unwrap();
        stream.readable().await.unwrap();
        let mut buf = [0; 2];
        assert_eq!(
            stream
                .try_read_vectored(&mut [IoSliceMut::new(&mut buf)])
                .unwrap(),
            2
        );
        assert_eq!(&buf, b"ab");

        stream.readable().await.unwrap();
        assert_eq!(stream.try_read(&mut buf).unwrap(), 0);
    });
}

#[test]
fn poll_ready_fns_match_async_fns() {
    fixture::lo(async {
        let (stream, mut peer) = pair(9104).await.unwrap();
        peer.write_u8(1).await.unwrap();
        peer.shutdown().await.unwrap();

        poll_fn(|cx| stream.poll_write_ready(cx)).await.unwrap();
        poll_fn(|cx| stream.poll_read_ready(cx)).await.unwrap();
        assert_eq!(stream.try_read(&mut [0; 1]).unwrap(), 1);
        poll_fn(|cx| stream.poll_read_ready(cx)).await.unwrap();
        assert!(stream
            .ready(Interest::READABLE)
            .await
            .unwrap()
            .is_read_closed());
    });
}

#[test]
fn halves_expose_readiness() {
    fixture::lo(async {
        let (stream, mut peer) = pair(9105).await.unwrap();
        let (read, write) = stream.into_split();
        write.writable().await.unwrap();
        peer.write_all(b"a").await.unwrap();
        read.readable().await.unwrap();
        assert!(read.ready(Interest::READABLE).await.unwrap().is_readable());
        assert!(write.ready(Interest::WRITABLE).await.unwrap().is_writable());

        let mut stream = read.reunite(write).unwrap();
        let (read, write) = stream.split();
        read.readable().await.unwrap();
        write.writable().await.unwrap();
    });
}

#[test]
fn async_waiters_release_task_wakers() {
    for method in WAITS {
        for cancel in [false, true] {
            fixture::lo_with_config(tiny(), async move {
                let (mut stream, mut peer) = pair(9106).await.unwrap();
                if cancel {
                    fill(&mut stream).await; // Neither direction is ready.
                } else {
                    peer.write_all(b"z").await.unwrap();
                    sleep(Duration::from_millis(10)).await; // Both are.
                }

                // Short-lived tasks on one stream must not leave task
                // wakers behind in the kernel.
                for _ in 0..32 {
                    let mut task = task::spawn(());
                    task.enter(|cx, _| {
                        let mut future = Box::pin(method.run(&stream));
                        if cancel {
                            assert_pending!(future.as_mut().poll(cx));
                        } else {
                            assert_ready_ok!(future.as_mut().poll(cx));
                        }
                    });
                    assert_eq!(task.waker_ref_count(), 1);
                }
            });
        }
    }
}

#[test]
fn cancellation_preserves_other_waiters() {
    for method in WAITS {
        fixture::lo_with_config(tiny(), async move {
            let (mut stream, mut peer) = pair(9107).await.unwrap();
            fill(&mut stream).await;
            let mut first = task::spawn(method.run(&stream));
            let mut second = task::spawn(method.run(&stream));
            let mut cancelled = task::spawn(method.run(&stream));
            assert_pending!(first.poll());
            assert_pending!(second.poll());
            assert_pending!(cancelled.poll());
            drop(cancelled);

            unblock(method, &mut peer).await;
            assert!(first.is_woken());
            assert!(second.is_woken());
            assert_ready_ok!(first.poll());
            assert_ready_ok!(second.poll());
            assert_eq!(first.waker_ref_count(), 1);
            assert_eq!(second.waker_ref_count(), 1);
        });
    }
}

#[test]
fn cancelling_one_future_in_a_task_preserves_the_other() {
    fixture::lo(async {
        let (stream, mut peer) = pair(9108).await.unwrap();
        let mut task = task::spawn(());
        let mut survivor = Box::pin(stream.readable());
        task.enter(|cx, _| {
            assert_pending!(survivor.as_mut().poll(cx));
            // Same waker; dropping this one must not unregister the other.
            let mut cancelled = Box::pin(stream.readable());
            assert_pending!(cancelled.as_mut().poll(cx));
        });

        peer.write_all(b"x").await.unwrap();
        sleep(Duration::from_millis(10)).await;
        assert!(task.is_woken());
        task.enter(|cx, _| assert_ready_ok!(survivor.as_mut().poll(cx)));
        drop(survivor);
        assert_eq!(task.waker_ref_count(), 1);
    });
}

#[derive(Clone, Copy)]
enum PollWait {
    Readable,
    Writable,
    Peek,
}

impl PollWait {
    async fn run(self, stream: &TcpStream) -> io::Result<()> {
        let mut bytes = [0; 1];
        let mut buf = ReadBuf::new(&mut bytes);
        poll_fn(|cx| match self {
            Self::Readable => stream.poll_read_ready(cx),
            Self::Writable => stream.poll_write_ready(cx),
            Self::Peek => stream.poll_peek(cx, &mut buf).map_ok(|_| ()),
        })
        .await
    }
}

#[test]
fn poll_slot_wakes_only_the_latest_caller() {
    for method in [PollWait::Readable, PollWait::Writable, PollWait::Peek] {
        fixture::lo_with_config(tiny(), async move {
            let (mut stream, mut peer) = pair(9109).await.unwrap();
            fill(&mut stream).await;
            let async_method = match method {
                PollWait::Writable => Wait::Writable,
                _ => Wait::Readable,
            };
            let mut old_poll = task::spawn(method.run(&stream));
            let mut latest_poll = task::spawn(method.run(&stream));
            let mut asynchronous = task::spawn(async_method.run(&stream));
            assert_pending!(old_poll.poll());
            assert_pending!(latest_poll.poll());
            assert_pending!(asynchronous.poll());

            unblock(async_method, &mut peer).await;
            assert!(!old_poll.is_woken());
            assert!(latest_poll.is_woken());
            assert!(asynchronous.is_woken());
            assert_ready_ok!(latest_poll.poll());
            assert_ready_ok!(asynchronous.poll());
        });
    }
}

#[test]
fn waiters_observe_connection_close() {
    for reset in [false, true] {
        fixture::lo_with_config(tiny(), async move {
            let (mut stream, mut peer) = pair(9110).await.unwrap();
            fill(&mut stream).await;
            let mut read = task::spawn(stream.readable());
            let mut write = task::spawn(stream.writable());
            let mut buf = [0];
            let mut peek = task::spawn(stream.peek(&mut buf));
            assert_pending!(read.poll());
            assert_pending!(write.poll());
            assert_pending!(peek.poll());

            if reset {
                drop(peer); // Unread data → RST.
            } else {
                peer.shutdown().await.unwrap();
            }
            sleep(Duration::from_millis(10)).await;
            assert!(read.is_woken());
            assert!(peek.is_woken());
            assert_ready_ok!(read.poll());
            if reset {
                assert!(write.is_woken());
                assert_ready_ok!(write.poll());
                let err = assert_ready!(peek.poll()).unwrap_err();
                assert_eq!(err.kind(), ErrorKind::ConnectionReset);
            } else {
                // FIN closes reads but frees no send buffer space.
                assert_pending!(write.poll());
                assert_eq!(assert_ready_ok!(peek.poll()), 0);
            }
        });
    }
}

#[test]
fn shared_stream_wakes_concurrent_waiters() {
    fixture::lo_with_config(tiny(), async {
        let (mut stream, mut peer) = pair(9111).await.unwrap();
        fill(&mut stream).await;
        let stream = Arc::new(stream);
        let w = stream.clone();
        let writable = tokio::spawn(async move { w.writable().await });
        let r = stream.clone();
        let readable = tokio::spawn(async move { r.readable().await });
        tokio::task::yield_now().await;

        assert_eq!(peer.read_u8().await.unwrap(), b'x');
        peer.write_all(b"z").await.unwrap();
        let (write, read) = timeout(Duration::from_secs(1), async {
            tokio::join!(writable, readable)
        })
        .await
        .unwrap();
        write.unwrap().unwrap();
        read.unwrap().unwrap();
    });
}

#[test]
fn udp_readiness() {
    fixture::lo(async {
        let a = UdpSocket::bind("127.0.0.1:9200").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        b.writable().await.unwrap();
        assert!(timeout(Duration::from_secs(1), a.readable()).await.is_err());

        b.send_to(b"hi", "127.0.0.1:9200").await.unwrap();
        a.readable().await.unwrap();
        assert!(a.ready(Interest::READABLE).await.unwrap().is_readable());
        poll_fn(|cx| a.poll_recv_ready(cx)).await.unwrap();
        poll_fn(|cx| a.poll_send_ready(cx)).await.unwrap();
        let mut buf = [0; 2];
        assert_eq!(a.try_recv(&mut buf).unwrap(), 2);
        assert!(timeout(Duration::from_secs(1), a.readable()).await.is_err());
    });
}

#[test]
fn lookup_host_resolves_like_to_socket_addrs() {
    fixture::lo(async {
        let addrs: Vec<_> = lookup_host("127.0.0.1:80").await.unwrap().collect();
        assert_eq!(addrs, ["127.0.0.1:80".parse().unwrap()]);
        assert_eq!(
            lookup_host("bogus:80").await.err().map(|e| e.kind()),
            Some(ErrorKind::NotFound)
        );
        assert_eq!(
            lookup_host("port_missing").await.err().map(|e| e.kind()),
            Some(ErrorKind::InvalidInput)
        );
    });
}
