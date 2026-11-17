use std::{
    future::{poll_fn, Future},
    io::{self, ErrorKind, IoSliceMut},
    time::Duration,
};
use test_case::test_case;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, Interest, ReadBuf},
    time::sleep,
};
use tokio_test::{assert_pending, assert_ready_ok, task};
use turmoil::{
    net::{tcp::OwnedReadHalf, TcpListener, TcpStream},
    Builder, Result,
};

/// Leave `stream` with nothing to read and no room to write: with
/// one-byte buffers, the first byte waits unread at the peer and the
/// second in our send buffer behind a zero window.
async fn fill(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(b"xy").await
}

async fn pair() -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind("127.0.0.1:1234").await?;
    let (stream, (peer, _)) =
        tokio::try_join!(TcpStream::connect("127.0.0.1:1234"), listener.accept())?;
    Ok((stream, peer))
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

#[test_case(1; "no_credits")]
#[test_case(2; "spare_credit")]
fn try_write_after_reset(capacity: usize) -> Result {
    let mut sim = Builder::new().tcp_capacity(capacity).build();
    sim.client("client", async {
        let (mut stream, peer) = pair().await?;
        // With one-byte buffers this leaves no space; with two, some.
        stream.write_all(b"xy").await?;
        sleep(Duration::from_secs(1)).await;
        drop(peer); // Unread data causes RST.
        sleep(Duration::from_secs(1)).await;

        assert!(stream.ready(Interest::WRITABLE).await?.is_write_closed());
        // Repeated writes must report the terminal error, regardless of space.
        for _ in 0..3 {
            assert_eq!(
                ErrorKind::ConnectionReset,
                stream.try_write(b"y").unwrap_err().kind()
            );
        }
        Ok(())
    });
    sim.run()
}

#[test_case(Wait::Readable, false; "completed_readable")]
#[test_case(Wait::Writable, false; "completed_writable")]
#[test_case(Wait::ReadyRead, false; "completed_ready_read")]
#[test_case(Wait::ReadyWrite, false; "completed_ready_write")]
#[test_case(Wait::ReadyBoth, false; "completed_ready_both")]
#[test_case(Wait::Peek, false; "completed_peek")]
#[test_case(Wait::Readable, true; "cancelled_readable")]
#[test_case(Wait::Writable, true; "cancelled_writable")]
#[test_case(Wait::ReadyRead, true; "cancelled_ready_read")]
#[test_case(Wait::ReadyWrite, true; "cancelled_ready_write")]
#[test_case(Wait::ReadyBoth, true; "cancelled_ready_both")]
#[test_case(Wait::Peek, true; "cancelled_peek")]
fn async_waiters_release_task_wakers(method: Wait, cancel: bool) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        if cancel {
            fill(&mut stream).await?; // Neither direction is ready.
        } else {
            peer.write_all(b"x").await?;
            sleep(Duration::from_secs(1)).await; // Both directions are ready.
        }

        // Distinct short-lived tasks on one stream must not accumulate wakers.
        for _ in 0..32 {
            let mut task = task::spawn(());
            task.enter(|cx, _| {
                let mut future = Box::pin(method.run(&stream));
                if cancel {
                    assert_pending!(future.as_mut().poll(cx));
                } else {
                    assert_ready_ok!(future.as_mut().poll(cx));
                }
                // Drop also cancels a pending future.
            });
            assert_eq!(1, task.waker_ref_count());
        }
        Ok(())
    });
    sim.run()
}

#[test_case(Wait::Readable; "readable")]
#[test_case(Wait::Writable; "writable")]
#[test_case(Wait::ReadyRead; "ready_read")]
#[test_case(Wait::ReadyWrite; "ready_write")]
#[test_case(Wait::ReadyBoth; "ready_both")]
#[test_case(Wait::Peek; "peek")]
fn cancellation_preserves_other_waiters(method: Wait) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        fill(&mut stream).await?;
        let mut first = task::spawn(method.run(&stream));
        let mut second = task::spawn(method.run(&stream));
        let mut cancelled = task::spawn(method.run(&stream));
        assert_pending!(first.poll());
        assert_pending!(second.poll());
        assert_pending!(cancelled.poll());
        drop(cancelled);

        if method.write_only() {
            assert_eq!(b'x', peer.read_u8().await?);
        } else {
            peer.write_all(b"y").await?;
        }
        sleep(Duration::from_secs(1)).await;
        // Checking wakeups before repolling catches stranded futures.
        assert!(first.is_woken());
        assert!(second.is_woken());
        assert_ready_ok!(first.poll());
        assert_ready_ok!(second.poll());
        assert_eq!(1, first.waker_ref_count());
        assert_eq!(1, second.waker_ref_count());
        Ok(())
    });
    sim.run()
}

#[test_case(Wait::Readable; "readable")]
#[test_case(Wait::Writable; "writable")]
#[test_case(Wait::ReadyRead; "ready_read")]
#[test_case(Wait::ReadyWrite; "ready_write")]
#[test_case(Wait::ReadyBoth; "ready_both")]
#[test_case(Wait::Peek; "peek")]
fn waiter_rearms_after_readiness_is_consumed(method: Wait) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        fill(&mut stream).await?;
        let mut waiter = task::spawn(method.run(&stream));
        assert_pending!(waiter.poll());

        for round in 0..2 {
            if method.write_only() {
                peer.read_u8().await?;
            } else {
                peer.write_all(b"y").await?;
            }
            sleep(Duration::from_secs(1)).await;
            assert!(waiter.is_woken());
            if round == 0 {
                // Another task consumes readiness before this waiter can run.
                if method.write_only() {
                    assert_eq!(1, stream.try_write(b"x")?);
                } else {
                    assert_eq!(
                        1,
                        stream.try_read_vectored(&mut [IoSliceMut::new(&mut [0; 1])])?
                    );
                }
                assert_pending!(waiter.poll());
            }
        }
        assert_ready_ok!(waiter.poll());
        Ok(())
    });
    sim.run()
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

#[test_case(PollWait::Readable, false; "poll_read_after_async")]
#[test_case(PollWait::Readable, true; "poll_read_before_async")]
#[test_case(PollWait::Writable, false; "poll_write_after_async")]
#[test_case(PollWait::Writable, true; "poll_write_before_async")]
#[test_case(PollWait::Peek, false; "poll_peek_after_async")]
#[test_case(PollWait::Peek, true; "poll_peek_before_async")]
fn poll_and_async_waiters_are_independent(method: PollWait, poll_first: bool) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        fill(&mut stream).await?;
        let async_method = if matches!(method, PollWait::Writable) {
            Wait::Writable
        } else {
            Wait::Readable
        };
        let mut old_poll = task::spawn(method.run(&stream));
        let mut latest_poll = task::spawn(method.run(&stream));
        let mut asynchronous = task::spawn(async_method.run(&stream));
        assert_pending!(old_poll.poll());
        if poll_first {
            assert_pending!(latest_poll.poll());
            assert_pending!(asynchronous.poll());
        } else {
            assert_pending!(asynchronous.poll());
            assert_pending!(latest_poll.poll());
        }

        if async_method.write_only() {
            peer.read_u8().await?;
        } else {
            peer.write_all(b"y").await?;
        }
        sleep(Duration::from_secs(1)).await;
        assert!(!old_poll.is_woken());
        assert!(latest_poll.is_woken());
        assert!(asynchronous.is_woken());
        assert_ready_ok!(latest_poll.poll());
        assert_ready_ok!(asynchronous.poll());
        Ok(())
    });
    sim.run()
}

#[test_case(false; "credits")]
#[test_case(true; "reset")]
fn completed_combined_ready_preserves_write_waiter(reset: bool) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        fill(&mut stream).await?;
        peer.write_all(b"z").await?;
        sleep(Duration::from_secs(1)).await;
        let mut blocked_writer = task::spawn(stream.writable());
        assert_pending!(blocked_writer.poll());

        let ready = stream
            .ready(Interest::READABLE | Interest::WRITABLE)
            .await?;
        assert!(ready.is_readable());
        assert!(!ready.is_writable());
        if reset {
            drop(peer);
            sleep(Duration::from_secs(1)).await;
            assert!(blocked_writer.is_woken());
            // As in tokio, readiness succeeds; the write reports the reset.
            assert_ready_ok!(blocked_writer.poll());
            drop(blocked_writer);
            assert_eq!(
                ErrorKind::ConnectionReset,
                stream.try_write(b"w").unwrap_err().kind()
            );
        } else {
            // Reading reopens the window, which drains our send buffer.
            peer.read_u8().await?;
            sleep(Duration::from_secs(1)).await;
            assert!(blocked_writer.is_woken());
            assert_ready_ok!(blocked_writer.poll());
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn cancelling_one_future_in_same_task_preserves_other_waiter() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async {
        let (stream, mut peer) = pair().await?;
        let mut task = task::spawn(());
        let mut survivor = Box::pin(stream.readable());
        task.enter(|cx, _| {
            assert_pending!(survivor.as_mut().poll(cx));
            let mut cancelled = Box::pin(stream.readable());
            assert_pending!(cancelled.as_mut().poll(cx));
            // Both futures use the same waker; dropping one must not remove
            // the other future's registration.
        });

        peer.write_all(b"x").await?;
        sleep(Duration::from_secs(1)).await;
        assert!(task.is_woken());
        task.enter(|cx, _| assert_ready_ok!(survivor.as_mut().poll(cx)));
        drop(survivor);
        assert_eq!(1, task.waker_ref_count());
        Ok(())
    });
    sim.run()
}

#[test_case(false; "fin")]
#[test_case(true; "reset")]
fn multiple_waiters_observe_connection_close(reset: bool) -> Result {
    let mut sim = Builder::new().tcp_capacity(1).build();
    sim.client("client", async move {
        let (mut stream, mut peer) = pair().await?;
        fill(&mut stream).await?; // Leaves unread data at the peer.
        sleep(Duration::from_secs(1)).await;
        let mut read_a = task::spawn(stream.readable());
        let mut read_b = task::spawn(stream.readable());
        let mut write_a = task::spawn(stream.writable());
        let mut write_b = task::spawn(stream.writable());
        let mut buf_a = [0];
        let mut buf_b = [0];
        let mut peek_a = task::spawn(stream.peek(&mut buf_a));
        let mut peek_b = task::spawn(stream.peek(&mut buf_b));
        assert_pending!(read_a.poll());
        assert_pending!(read_b.poll());
        assert_pending!(write_a.poll());
        assert_pending!(write_b.poll());
        assert_pending!(peek_a.poll());
        assert_pending!(peek_b.poll());

        if reset {
            drop(peer);
        } else {
            peer.shutdown().await?;
        }
        sleep(Duration::from_secs(1)).await;
        assert!(read_a.is_woken());
        assert!(read_b.is_woken());
        assert!(peek_a.is_woken());
        assert!(peek_b.is_woken());
        assert_ready_ok!(read_a.poll());
        assert_ready_ok!(read_b.poll());
        if reset {
            assert!(write_a.is_woken());
            assert!(write_b.is_woken());
            // As in tokio, readiness succeeds; I/O reports the reset.
            assert_ready_ok!(write_a.poll());
            assert_ready_ok!(write_b.poll());
            for result in [
                tokio_test::assert_ready!(peek_a.poll()),
                tokio_test::assert_ready!(peek_b.poll()),
            ] {
                assert_eq!(ErrorKind::ConnectionReset, result.unwrap_err().kind());
            }
        } else {
            // FIN closes reads but frees no send buffer space.
            assert!(!write_a.is_woken());
            assert!(!write_b.is_woken());
            assert_pending!(write_a.poll());
            assert_pending!(write_b.poll());
            assert_eq!(0, assert_ready_ok!(peek_a.poll()));
            assert_eq!(0, assert_ready_ok!(peek_b.poll()));
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn owned_read_half_poll_peek_accepts_mut_reference() -> Result {
    let mut sim = Builder::new().build();
    sim.client("client", async {
        let (stream, mut peer) = pair().await?;
        let (mut reader, _writer) = stream.into_split();
        let mut bytes = [0; 1];
        let mut buf = ReadBuf::new(&mut bytes);
        // The explicit call checks the receiver type without auto-dereferencing
        // a Pin. It must accept the same &mut receiver as Tokio.
        let mut peek = task::spawn(poll_fn(|cx| {
            OwnedReadHalf::poll_peek(&mut reader, cx, &mut buf)
        }));
        assert_pending!(peek.poll());
        peer.write_all(b"x").await?;
        sleep(Duration::from_secs(1)).await;
        assert!(peek.is_woken());
        assert_eq!(1, assert_ready_ok!(peek.poll()));
        drop(peek);
        assert_eq!(b"x", buf.filled());
        // Peeking preserves the data for the next read.
        assert_eq!(b'x', reader.read_u8().await?);

        peer.shutdown().await?;
        let mut buf = ReadBuf::new(&mut bytes);
        assert_eq!(0, poll_fn(|cx| reader.poll_peek(cx, &mut buf)).await?);
        Ok(())
    });
    sim.run()
}
