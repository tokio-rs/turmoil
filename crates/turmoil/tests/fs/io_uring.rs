//! Smoke tests for [`turmoil::io_uring`].
//!
//! Exercises the core ring data flow: push SQE, submit, drain CQE,
//! observe the side effects landing in the simulated fs. Out-of-order
//! completion, fault wiring, and crash semantics are covered in their
//! own task-specific modules.

use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use test_case::test_case;
use turmoil::fs::shim::std::fs::{create_dir_all, OpenOptions};
use turmoil::io_uring::{cqueue, opcode, squeue, types, AsyncFd, IoUring};
use turmoil::{Builder, Result};

use super::durability::restart;

const TEST_DIR: &str = "/uring";

fn open_rw(path: &str) -> std::io::Result<turmoil::fs::shim::std::fs::File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
}

#[test]
fn write_then_read_via_ring() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/data"))?;
        let fd = types::Fd(file.as_raw_fd());

        let mut ring = IoUring::new(8).expect("new ring");

        // Submit a Write.
        let payload = b"hello uring".to_vec();
        let write = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .offset(0)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&write).expect("push write");
        }
        ring.submit().expect("submit write");

        // Drain its CQE.
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), payload.len() as i32);

        // Submit a Read into a fresh buffer.
        let mut buf = vec![0u8; payload.len()];
        let read = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .offset(0)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&read).expect("push read");
        }
        ring.submit().expect("submit read");

        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 2);
        assert_eq!(cqe.result(), payload.len() as i32);
        assert_eq!(buf, payload);

        // Hold the file alive across the ops to keep its fd live.
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn fsync_returns_zero() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/sync"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        let entry = opcode::Fsync::new(fd).build().user_data(7);
        unsafe {
            ring.submission().push(&entry).expect("push fsync");
        }
        ring.submit().expect("submit fsync");

        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 7);
        assert_eq!(cqe.result(), 0);
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn ring_fsync_makes_ring_writes_durable() -> Result {
    // Pure-ring durability: write via ring, fsync via ring (no shim
    // sync), crash + bounce, expect data to survive. The mix_*
    // tests cover ring+shim cross-flush; this test pins the
    // ring-only path so a regression in exec_fsync's wiring (e.g.
    // calling sync_data instead of sync_file, or skipping the
    // sync_dir step) shows up.
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let phase_host = phase.clone();
    let notify_host = notify.clone();

    sim.host("server", move || {
        let phase = phase_host.clone();
        let notify = notify_host.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all(TEST_DIR)?;
                turmoil::fs::shim::std::fs::sync_dir("/")?;
                let file = open_rw(&format!("{TEST_DIR}/ring-durable"))?;
                let fd = types::Fd(file.as_raw_fd());
                let mut ring = IoUring::new(4).expect("ring");

                let payload = b"ring-durable".to_vec();
                let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
                    .build()
                    .user_data(1);
                unsafe {
                    ring.submission().push(&w).expect("push w");
                }
                ring.submit().expect("submit w");
                let _ = drain_one(&mut ring).await;

                let f = opcode::Fsync::new(fd).build().user_data(2);
                unsafe {
                    ring.submission().push(&f).expect("push f");
                }
                ring.submit().expect("submit f");
                let cqe = drain_one(&mut ring).await;
                assert_eq!(cqe.result(), 0);
                turmoil::fs::shim::std::fs::sync_dir(TEST_DIR)?;
                std::mem::forget(file);
            } else {
                let file = OpenOptions::new()
                    .read(true)
                    .open(format!("{TEST_DIR}/ring-durable"))?;
                let mut buf = [0u8; 12];
                file.read_at(&mut buf, 0)?;
                assert_eq!(&buf, b"ring-durable");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("phase0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("phase1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn many_concurrent_reads_complete_in_shuffled_order() -> Result {
    // Submits N reads; verifies all N CQEs come back with correct
    // user_data and result, regardless of order. The order itself is
    // RNG-shuffled inside the simulation, so we only check coverage.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let path = format!("{TEST_DIR}/many");
        let file = open_rw(&path)?;
        let fd = types::Fd(file.as_raw_fd());

        // Seed the file with content via a single ring write.
        let payload: Vec<u8> = (0..(64 * 8)).map(|i| (i & 0xff) as u8).collect();
        let mut ring = IoUring::new(64).expect("new ring");
        let write = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(0);
        unsafe {
            ring.submission().push(&write).expect("push seed");
        }
        ring.submit().expect("submit seed");
        let _ = drain_one(&mut ring).await;

        // Submit 64 reads at distinct offsets, distinct user_data tags.
        let mut bufs: Vec<Vec<u8>> = (0..64).map(|_| vec![0u8; 8]).collect();
        for (i, buf) in bufs.iter_mut().enumerate() {
            let entry = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                .offset((i * 8) as u64)
                .build()
                .user_data(100 + i as u64);
            unsafe {
                ring.submission().push(&entry).expect("push read");
            }
        }
        ring.submit().expect("submit reads");

        // Drain all 64; record the arrival order.
        let mut seen: Vec<u64> = Vec::new();
        for _ in 0..64 {
            let cqe = drain_one(&mut ring).await;
            seen.push(cqe.user_data());
        }

        // (a) Coverage: every submitted user_data shows up exactly once.
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        let expected: Vec<u64> = (100..164).collect();
        assert_eq!(sorted, expected);

        // (b) Out-of-order: the simulation shuffles matured CQEs with
        // the seed RNG, so arrival order MUST differ from submission
        // order at least somewhere. A FIFO regression would fail
        // here. (Probability of all 64 landing in submission order
        // by accident is 1/64! — vanishingly small.)
        assert_ne!(
            seen, expected,
            "CQEs delivered in submission order; sim should shuffle"
        );

        // Spot-check buffer content of two reads to confirm payloads
        // weren't garbled by the reorder.
        assert_eq!(bufs[0][0], 0);
        assert_eq!(bufs[10][0], (10 * 8) as u8);
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn read_with_invalid_fd_returns_ebadf() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let mut ring = IoUring::new(4).expect("new ring");
        let mut buf = [0u8; 4];
        let bogus = types::Fd(999_999_999); // outside SIM_FD_BASE range
        let entry = opcode::Read::new(bogus, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(11);
        unsafe {
            ring.submission().push(&entry).expect("push read");
        }
        ring.submit().expect("submit read");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 11);
        assert_eq!(cqe.result(), -9, "expected -EBADF");
        Ok(())
    });
    sim.run()
}

#[test]
fn o_direct_misaligned_read_yields_einval() -> Result {
    // O_DIRECT requires ptr/offset/len to be multiples of
    // direct_io_alignment. Real Linux returns -EINVAL for misaligned
    // ops; the simulation surfaces the same.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        // direct_io_alignment defaults to 512; ask for direct I/O.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .direct_io(true)
            .open(format!("{TEST_DIR}/odirect"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // 7-byte buffer at non-page-aligned offset: violates all three
        // alignment requirements.
        let mut buf = [0u8; 7];
        let entry = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .offset(1)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&entry).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(
            cqe.result(),
            -22,
            "expected -EINVAL for misaligned O_DIRECT"
        );
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn submit_with_args_rejects_bad_timespec() -> Result {
    // tv_nsec must be < 1_000_000_000. Real-kernel io_uring_enter
    // returns EINVAL on a bad timespec; the simulation should match.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let ring = IoUring::new(2).expect("new ring");
        let bogus = types::Timespec::new().sec(1).nsec(1_500_000_000);
        let args = types::SubmitArgs::new().timespec(&bogus);
        let res = ring.submitter().submit_with_args(0, &args);
        match res {
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn io_link_flag_yields_einval() -> Result {
    // IOSQE_IO_LINK is one of the SQE flags the simulation rejects.
    // Submitting an SQE with the flag set must surface as -EINVAL on
    // its CQE (same as the real kernel's behavior for an unsupported
    // op flag).
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/link"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");
        let mut buf = [0u8; 4];
        let entry = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(1)
            .flags(squeue::Flags::IO_LINK);
        unsafe {
            ring.submission().push(&entry).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), -22, "expected -EINVAL for IO_LINK");
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn async_cancel_targets_inflight_op() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(std::time::Duration::from_millis(10))
        .max_latency(std::time::Duration::from_millis(10));
    let mut sim = builder.build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/cancel"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // Stage a slow read (10ms).
        let mut buf = vec![0u8; 8];
        let read = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(20);
        unsafe {
            ring.submission().push(&read).expect("push read");
        }
        ring.submit().expect("submit read");

        // Cancel by user_data before time advances enough for the read
        // to mature.
        let cancel = opcode::AsyncCancel::new(20).build().user_data(21);
        unsafe {
            ring.submission().push(&cancel).expect("push cancel");
        }
        ring.submit().expect("submit cancel");

        // Drain both CQEs. Order is shuffled.
        let mut by_ud = std::collections::HashMap::new();
        for _ in 0..2 {
            let cqe = drain_one(&mut ring).await;
            by_ud.insert(cqe.user_data(), cqe.result());
        }
        assert_eq!(by_ud.get(&20), Some(&-125), "read should be -ECANCELED");
        assert_eq!(by_ud.get(&21), Some(&0), "cancel itself succeeds");
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn io_error_probability_surfaces_eio() -> Result {
    let mut builder = Builder::new();
    builder.fs().io_error_probability(1.0);
    let mut sim = builder.build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/eio"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // Read should error. Use a tiny buffer so the slice is valid
        // regardless of file content.
        let mut buf = [0u8; 4];
        let read = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&read).expect("push read");
            ring.submission().push(&opcode::Fsync::new(fd).build())?;
        }
        ring.submit().expect("submit");
        for _ in 0..2 {
            let cqe = drain_one(&mut ring).await;
            assert_eq!(cqe.result(), -5, "expected -EIO on forced io_error");
        }
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn corruption_probability_flips_a_byte() -> Result {
    let mut builder = Builder::new();
    builder.fs().corruption_probability(1.0);
    let mut sim = builder.build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/corrupt"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // Seed via ring write so the test exercises the same path.
        let payload = b"the quick brown fox".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push w");
        }
        ring.submit().expect("submit w");
        let _ = drain_one(&mut ring).await;

        // Read it back; at least one byte should differ.
        let mut buf = vec![0u8; payload.len()];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&r).expect("push r");
        }
        ring.submit().expect("submit r");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.result(), payload.len() as i32);
        assert_ne!(buf, payload, "100% corruption should flip a byte");
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn short_read_probability_truncates() -> Result {
    let mut builder = Builder::new();
    builder.fs().short_read_probability(1.0);
    let mut sim = builder.build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/short"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // Seed file with more than 1 byte so short_read can fire (the
        // shim only shortens when n > 1).
        let payload = b"abcdef".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push w");
        }
        ring.submit().expect("submit w");
        let _ = drain_one(&mut ring).await;

        let mut buf = vec![0u8; payload.len()];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&r).expect("push r");
        }
        ring.submit().expect("submit r");
        let cqe = drain_one(&mut ring).await;
        assert!(
            cqe.result() > 0 && (cqe.result() as usize) < payload.len(),
            "expected short read; got {}",
            cqe.result()
        );
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn crash_drops_unsynced_writes_submitted_via_ring() -> Result {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    // Phase 0: write via ring without fsync -> not durable.
    // Phase 1: after crash + bounce, the file should be gone.
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let phase_host = phase.clone();
    let notify_host = notify.clone();

    sim.host("server", move || {
        let phase = phase_host.clone();
        let notify = notify_host.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all(TEST_DIR)?;
                turmoil::fs::shim::std::fs::sync_dir("/")?;
                let file = open_rw(&format!("{TEST_DIR}/lost"))?;
                let fd = types::Fd(file.as_raw_fd());
                let mut ring = IoUring::new(4).expect("new ring");

                let payload = b"unsynced via ring".to_vec();
                let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
                    .build()
                    .user_data(1);
                unsafe {
                    ring.submission().push(&w).expect("push w");
                }
                ring.submit().expect("submit w");
                // Drain so the write side effect actually lands in the
                // file (without fsync, it's still unsynced and will be
                // lost on crash).
                let _ = drain_one(&mut ring).await;
                // Deliberately NO fsync.
                std::mem::forget(file); // keep fd alive across crash test
            } else {
                // Phase 1: file should not exist (open without create).
                let exists = OpenOptions::new()
                    .read(true)
                    .open(format!("{TEST_DIR}/lost"))
                    .is_ok();
                assert!(!exists, "unsynced ring write should not survive crash");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });

    let n = notify.clone();
    sim.client("phase0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("phase1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn crash_drops_inflight_ops_so_no_cqes() -> Result {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    // Submit an op with non-zero latency so it stays in-flight; crash;
    // verify the post-bounce host can't observe a CQE for it.
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(std::time::Duration::from_secs(1))
        .max_latency(std::time::Duration::from_secs(1));
    let mut sim = builder.build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let phase_host = phase.clone();
    let notify_host = notify.clone();

    sim.host("server", move || {
        let phase = phase_host.clone();
        let notify = notify_host.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all(TEST_DIR)?;
                turmoil::fs::shim::std::fs::sync_dir("/")?;
                let file = open_rw(&format!("{TEST_DIR}/inflight"))?;
                let fd = types::Fd(file.as_raw_fd());
                let mut ring = IoUring::new(4).expect("new ring");

                // Stage a 1-second-latency read.
                let mut buf = vec![0u8; 8];
                let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
                    .build()
                    .user_data(99);
                unsafe {
                    ring.submission().push(&r).expect("push r");
                }
                ring.submit().expect("submit r");
                // Don't wait for completion — let it stay in-flight.
                std::mem::forget(file);
            } else {
                // Phase 1: a fresh ring observes no stale CQEs even
                // after a sync — there's nothing in the ring to expose.
                let mut ring = IoUring::new(4).expect("new ring after bounce");
                let mut cq = ring.completion();
                cq.sync();
                assert!(
                    cq.next().is_none(),
                    "fresh ring after bounce should be empty"
                );
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });

    let n = notify.clone();
    sim.client("phase0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("phase1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn async_fd_readable_resolves_when_cqe_matures() -> Result {
    // Submit a Read with non-zero latency, then await AsyncFd::readable.
    // It must resolve at simulated time = configured latency, without
    // spinning.
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(std::time::Duration::from_millis(50))
        .max_latency(std::time::Duration::from_millis(50));
    let mut sim = builder.build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/asyncfd"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");
        let async_fd = AsyncFd::new(RingFdHandle(<IoUring as std::os::fd::AsRawFd>::as_raw_fd(
            &ring,
        )))
        .expect("AsyncFd");

        // Seed via the same ring (drained before the AsyncFd test so
        // the AsyncFd round only sees the latency-bearing read).
        let payload = b"async ready".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push w");
        }
        ring.submit().expect("submit w");
        let _ = drain_one(&mut ring).await;

        // Stage a read with 50ms latency.
        let mut buf = vec![0u8; 11];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&r).expect("push r");
        }
        ring.submit().expect("submit r");

        let start = tokio::time::Instant::now();
        let _guard = async_fd.readable().await.expect("readable");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(50),
            "AsyncFd::readable resolved before latency: {:?}",
            elapsed
        );

        let mut cq = ring.completion();
        cq.sync();
        let cqe = cq.next().expect("CQE ready");
        assert_eq!(cqe.user_data(), 2);
        assert_eq!(cqe.result(), 11);
        drop(file);
        Ok(())
    });
    sim.run()
}

// --- Cross-path integration with the regular fs shim ----------------
//
// io_uring does not maintain a parallel filesystem. Reads and writes
// submitted via a ring land in the same per-host `Fs` state the
// shim::std::fs and shim::tokio::fs surfaces use. These tests pin the
// contract: writes from one path are visible to reads on the other,
// either fsync route flushes either kind of pending write, and a
// crash drops everything that wasn't synced regardless of which API
// produced it.

#[test]
fn mix_shim_write_then_ring_read() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/mix1"))?;
        let fd = types::Fd(file.as_raw_fd());
        file.write_all_at(b"shim wrote this", 0)?;

        let mut ring = IoUring::new(4).expect("ring");
        let mut buf = vec![0u8; 15];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&r).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.result(), 15);
        assert_eq!(buf, b"shim wrote this");
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn mix_ring_write_then_shim_read() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/mix2"))?;
        let fd = types::Fd(file.as_raw_fd());

        let mut ring = IoUring::new(4).expect("ring");
        let payload = b"ring wrote this".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.result(), payload.len() as i32);

        let mut buf = [0u8; 15];
        let n = file.read_at(&mut buf, 0)?;
        assert_eq!(n, payload.len());
        assert_eq!(&buf, payload.as_slice());
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn mix_interleaved_writes_show_latest() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/mix3"))?;
        let fd = types::Fd(file.as_raw_fd());
        file.write_all_at(b"AAAAAAAAAA", 0)?;

        let mut ring = IoUring::new(4).expect("ring");
        let payload = b"BBBBBB".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .offset(2)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push");
        }
        ring.submit().expect("submit");
        let _ = drain_one(&mut ring).await;

        // Shim overwrites bytes 5..7 with C's. Final layout:
        //   AA BBB CC BAA   (10 bytes)
        file.write_all_at(b"CC", 5)?;

        let mut buf = [0u8; 10];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&r).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.result(), 10);
        assert_eq!(&buf, b"AABBBCCBAA");
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn mix_shim_sync_all_flushes_ring_writes() -> Result {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let phase_host = phase.clone();
    let notify_host = notify.clone();

    sim.host("server", move || {
        let phase = phase_host.clone();
        let notify = notify_host.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all(TEST_DIR)?;
                turmoil::fs::shim::std::fs::sync_dir("/")?;
                let file = open_rw(&format!("{TEST_DIR}/mix4"))?;
                let fd = types::Fd(file.as_raw_fd());

                let mut ring = IoUring::new(4).expect("ring");
                let payload = b"durable via shim sync".to_vec();
                let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
                    .build()
                    .user_data(1);
                unsafe {
                    ring.submission().push(&w).expect("push");
                }
                ring.submit().expect("submit");
                let _ = drain_one(&mut ring).await;

                // Sync via the SHIM. Must flush the ring-issued write.
                file.sync_all()?;
                turmoil::fs::shim::std::fs::sync_dir(TEST_DIR)?;
                std::mem::forget(file);
            } else {
                let file = OpenOptions::new()
                    .read(true)
                    .open(format!("{TEST_DIR}/mix4"))?;
                let mut buf = [0u8; 21];
                file.read_at(&mut buf, 0)?;
                assert_eq!(&buf, b"durable via shim sync");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("phase0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("phase1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn mix_ring_fsync_flushes_shim_writes() -> Result {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let phase_host = phase.clone();
    let notify_host = notify.clone();

    sim.host("server", move || {
        let phase = phase_host.clone();
        let notify = notify_host.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all(TEST_DIR)?;
                turmoil::fs::shim::std::fs::sync_dir("/")?;
                let file = open_rw(&format!("{TEST_DIR}/mix5"))?;
                let fd = types::Fd(file.as_raw_fd());

                file.write_all_at(b"durable via ring fsync", 0)?;

                // Sync via the RING. Must flush the shim-issued write.
                let mut ring = IoUring::new(4).expect("ring");
                let f = opcode::Fsync::new(fd).build().user_data(1);
                unsafe {
                    ring.submission().push(&f).expect("push");
                }
                ring.submit().expect("submit");
                let cqe = drain_one(&mut ring).await;
                assert_eq!(cqe.result(), 0);
                turmoil::fs::shim::std::fs::sync_dir(TEST_DIR)?;
                std::mem::forget(file);
            } else {
                let file = OpenOptions::new()
                    .read(true)
                    .open(format!("{TEST_DIR}/mix5"))?;
                let mut buf = [0u8; 22];
                file.read_at(&mut buf, 0)?;
                assert_eq!(&buf, b"durable via ring fsync");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("phase0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("phase1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

// --- Coverage for previously-missing claims --------------------------

#[test]
fn async_cancel_of_matured_op_drops_buffer_safely() -> Result {
    // Pins the UAF fix in RingState::cancel: an op whose `when` has
    // already elapsed must still be cancelable, with its raw-pointer
    // payload dropped before we'd dereference it. We submit a Read
    // with zero latency, advance time so the op matures, then cancel
    // by user_data. The cancel CQE must come back with result=0
    // (target found in either inflight or ready) and the target's
    // CQE must be -ECANCELED.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/cancel-matured"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("ring");

        // Seed file so a read would have content.
        let payload = b"abcdefgh".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push w");
        }
        ring.submit().expect("submit w");
        let _ = drain_one(&mut ring).await;

        // Submit the read at zero latency so it matures immediately.
        let mut buf = vec![0u8; payload.len()];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(50);
        unsafe {
            ring.submission().push(&r).expect("push r");
        }
        ring.submit().expect("submit r");

        // Yield once to let the runtime advance simulated time so the
        // read matures into `inflight`/`ready` before the cancel runs.
        tokio::task::yield_now().await;

        // Cancel by user_data.
        let c = opcode::AsyncCancel::new(50).build().user_data(51);
        unsafe {
            ring.submission().push(&c).expect("push c");
        }
        ring.submit().expect("submit c");

        let mut by_ud = std::collections::HashMap::new();
        for _ in 0..2 {
            let cqe = drain_one(&mut ring).await;
            by_ud.insert(cqe.user_data(), cqe.result());
        }
        // Outcome depends on whether the read had time to mature
        // before the cancel. Both outcomes are acceptable: either the
        // cancel hit it (result 0, target -ECANCELED) or it raced
        // past completion (cancel sees -ENOENT, target reports the
        // real read result). The point of the test is that NEITHER
        // outcome involves a UAF — the slot was either dropped
        // before we'd dereference its ptr (the cancel-hit branch) or
        // executed before the cancel arrived (the race-past branch).
        let read_res = by_ud.get(&50).copied().expect("target CQE");
        let cancel_res = by_ud.get(&51).copied().expect("cancel CQE");
        assert!(
            (cancel_res == 0 && read_res == -125)
                || (cancel_res == -2 && read_res == payload.len() as i32),
            "unexpected pair: cancel={cancel_res} read={read_res}"
        );
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn async_cancel_of_unknown_user_data_returns_enoent() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let mut ring = IoUring::new(4).expect("ring");
        let c = opcode::AsyncCancel::new(9999).build().user_data(1);
        unsafe {
            ring.submission().push(&c).expect("push");
        }
        ring.submit().expect("submit");
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), -2, "expected -ENOENT for unknown user_data");
        Ok(())
    });
    sim.run()
}

#[test]
fn builder_setup_sqpoll_rejected() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let res = IoUring::builder().setup_sqpoll(0).build(4);
        match res {
            Ok(_) => panic!("expected InvalidInput, got Ok"),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("expected InvalidInput, got {e:?}"),
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn builder_setup_iopoll_rejected() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let res = IoUring::builder().setup_iopoll().build(4);
        match res {
            Ok(_) => panic!("expected InvalidInput, got Ok"),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {}
            Err(e) => panic!("expected InvalidInput, got {e:?}"),
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn async_fd_on_non_ring_fd_errors() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        // Open a regular file; its fd is allocated from SIM_FD_BASE
        // but is NOT registered in io_uring_rings. AsyncFd::new
        // should reject it.
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/not-a-ring"))?;
        let res = AsyncFd::new(RingFdHandle(file.as_raw_fd()));
        match res {
            Ok(_) => panic!("expected NotFound, got Ok"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("expected NotFound, got {e:?}"),
        }
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn rings_are_isolated_per_host() {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;

    // Each host opens a ring, records its ring fd, and notifies. fds
    // must be allocated independently per host (both should land at
    // the same starting ring fd since each host has its own
    // `next_ring_fd` counter on its IoUringHostState).
    let mut sim = Builder::new().build();
    let h1_fd = Arc::new(AtomicI32::new(0));
    let h2_fd = Arc::new(AtomicI32::new(0));
    let n1 = Arc::new(Notify::new());
    let n2 = Arc::new(Notify::new());

    let f1 = h1_fd.clone();
    let s1 = n1.clone();
    sim.host("h1", move || {
        let f = f1.clone();
        let s = s1.clone();
        async move {
            let ring = IoUring::new(4).expect("ring");
            f.store(<IoUring as AsRawFd>::as_raw_fd(&ring), Ordering::SeqCst);
            s.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let f2 = h2_fd.clone();
    let s2 = n2.clone();
    sim.host("h2", move || {
        let f = f2.clone();
        let s = s2.clone();
        async move {
            let ring = IoUring::new(4).expect("ring");
            f.store(<IoUring as AsRawFd>::as_raw_fd(&ring), Ordering::SeqCst);
            s.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    sim.client("test", async move {
        n1.notified().await;
        n2.notified().await;
        // Both hosts allocated the same fd integer because they have
        // separate ring registries. Confirms registries are not
        // globally shared. The exact value (SIM_FD_BASE +
        // IO_URING_FD_OFFSET) is an implementation detail; what
        // matters is that h1 and h2 agree on it.
        let h1 = h1_fd.load(Ordering::SeqCst);
        let h2 = h2_fd.load(Ordering::SeqCst);
        assert_eq!(h1, h2);
        assert!(h1 >= 1 << 30, "ring fd should live above SIM_FD_BASE");
        Ok(())
    });
    sim.run().expect("sim");
}

#[tokio::test]
async fn ring_handles_reject_another_hosts_registry() -> Result {
    use std::io::ErrorKind;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use turmoil::fs::Fs;
    use turmoil::io_uring::host::{enter, EnterCtx, IoUringHostState};

    let registry = || Arc::new(Mutex::new(IoUringHostState::new()));
    let (a, b) = (registry(), registry());
    let ctx = || EnterCtx {
        now: Duration::ZERO,
    };
    let _fs_a = Fs::default().enter();
    let entered_a = enter(&a, ctx());
    let ring = IoUring::new(4)?;
    let entry = opcode::AsyncCancel::new(99).build();
    let ready = AsyncFd::new(RingFdHandle(ring.as_raw_fd()))?;
    // One producer and one consumer, both obtained while A is entered.
    let mut sq = unsafe { ring.submission_shared() };
    let mut cq = unsafe { ring.completion_shared() };
    let submitter = ring.submitter();
    unsafe { sq.push(&entry)? };
    submitter.submit()?;
    cq.sync();
    assert_eq!(cq.len(), 1);

    let _fs_b = Fs::default().enter();
    let _entered_b = enter(&b, ctx());
    let mut local = IoUring::new(4)?;
    assert_eq!(local.as_raw_fd(), ring.as_raw_fd());
    unsafe { local.submission().push(&entry.clone().user_data(42))? };
    local.submit()?;
    assert!(cq.next().is_none());
    cq.sync();
    assert!(cq.is_empty());
    assert!(unsafe { sq.push(&entry) }.is_err());
    assert_eq!(submitter.submit().unwrap_err().kind(), ErrorKind::NotFound);
    assert!(matches!(ready.readable().await, Err(e) if e.kind() == ErrorKind::NotFound));
    drop(ring);
    assert_eq!(local.submit()?, 0);
    let mut cq = local.completion();
    cq.sync();
    assert_eq!(cq.next().unwrap().user_data(), 42);
    drop(_entered_b);
    drop(entered_a);
    Ok(())
}

#[test]
fn ring_handle_can_outlive_the_host() -> std::io::Result<()> {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use turmoil::io_uring::host::{enter, EnterCtx, IoUringHostState};

    let host = Arc::new(Mutex::new(IoUringHostState::new()));
    let owner = Arc::downgrade(&host);
    let ring = {
        let _guard = enter(
            &host,
            EnterCtx {
                now: Duration::ZERO,
            },
        );
        IoUring::new(4)?
    };
    drop(host);
    assert!(owner.upgrade().is_none());
    drop(ring);
    Ok(())
}

#[test]
fn zero_length_transfers_accept_null_buffers_after_fd_validation() -> Result {
    use std::ptr;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let file = open_rw("/file")?;
        let mut ring = IoUring::new(4)?;
        for (raw, expected) in [(file.as_raw_fd(), 0), (-1, -9)] {
            let fd = types::Fd(raw);
            let read = opcode::Read::new(fd, ptr::null_mut(), 0).build();
            let write = opcode::Write::new(fd, ptr::null(), 0).build();
            unsafe {
                ring.submission().push(&read)?;
                ring.submission().push(&write)?;
            }
            ring.submit()?;
            for _ in 0..2 {
                assert_eq!(drain_one(&mut ring).await.result(), expected);
            }
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn completion_queue_yields_nothing_without_sync() -> Result {
    // Real Linux: `next()` reads against the userspace head pointer,
    // which is only updated by `sync()`. A consumer that submits an
    // op and iterates `completion()` without calling `sync()` first
    // will see an empty CQ even though the kernel has posted a CQE.
    // The simulation enforces the same contract so the bug doesn't
    // hide under turmoil and bite on Linux.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all(TEST_DIR)?;
        let file = open_rw(&format!("{TEST_DIR}/no-sync"))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("new ring");

        // Submit + drain a write so the CQ has a matured CQE waiting.
        let payload = b"x".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), 1)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).expect("push");
        }
        ring.submit().expect("submit");

        // Without sync(): next() must return None even though there's
        // a matured CQE in the ring.
        {
            let mut cq = ring.completion();
            assert!(
                cq.next().is_none(),
                "next() without sync() must yield nothing"
            );
        }

        // After sync(): next() yields the CQE.
        let mut cq = ring.completion();
        cq.sync();
        let cqe = cq.next().expect("after sync, CQE should appear");
        assert_eq!(cqe.user_data(), 1);

        // Same handle, no second sync(): next() returns None even if
        // more matured CQEs exist (we only exposed one with the prior
        // sync). Submit another op to give the ring a CQE.
        let w2 = opcode::Write::new(fd, payload.as_ptr(), 1)
            .offset(1)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&w2).expect("push 2");
        }
        ring.submit().expect("submit 2");
        let mut cq = ring.completion();
        // Snapshot is empty before sync.
        assert_eq!(cq.len(), 0);
        cq.sync();
        assert_eq!(cq.len(), 1);
        let cqe = cq.next().expect("second CQE");
        assert_eq!(cqe.user_data(), 2);
        // After draining the snapshot, next() returns None until next sync.
        assert!(cq.next().is_none());
        drop(file);
        Ok(())
    });
    sim.run()
}

#[test]
fn async_fd_readable_wakes_when_ring_dropped() -> Result {
    // Drop semantics: an AsyncFd::readable() future awaiting on a
    // ring whose IoUring is dropped must wake (rather than sleep
    // until its cached deadline). The subsequent snapshot returns
    // NotFound.
    use tokio::time::{timeout, Duration as TD};
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let async_fd = {
            let ring = IoUring::new(4).expect("ring");
            let fd = <IoUring as AsRawFd>::as_raw_fd(&ring);
            let async_fd = AsyncFd::new(RingFdHandle(fd)).expect("AsyncFd");
            // Drop the ring while async_fd is alive.
            drop(ring);
            async_fd
        };
        // readable() must return Err(NotFound) promptly. Bound the
        // wait so a regression that sleeps forever shows up as a
        // timeout rather than a hang.
        let res = timeout(TD::from_secs(60), async_fd.readable())
            .await
            .expect("readable should not hang past deadline");
        match res {
            Ok(_) => panic!("expected NotFound after ring drop, got Ok"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("expected NotFound after ring drop, got {e:?}"),
        }
        Ok(())
    });
    sim.run()
}

#[test]
fn rejected_ring_transfers_leave_the_file_and_cache_untouched() -> Result {
    use std::time::Duration;
    use turmoil::fs::shim::std::fs::{write, File};
    use turmoil::fs::shim::tokio::fs as tokio_fs;

    let mut builder = Builder::new();
    let latency = Duration::from_millis(10);
    let timing = builder.fs().io_latency();
    timing.min_latency(latency).max_latency(latency);
    builder.fs().page_cache();
    let mut sim = builder.build();
    sim.client("c", async move {
        write("/file", b"data")?;
        let file = File::open("/file")?;
        let writer = OpenOptions::new().write(true).open("/file")?;
        let mut ring = IoUring::new(4)?;

        let payload = b"XXXX";
        let mut buf = [0; 4];
        let ro = types::Fd(file.as_raw_fd());
        let wo = types::Fd(writer.as_raw_fd());
        let w = opcode::Write::new(ro, payload.as_ptr(), 4).build();
        let r = opcode::Read::new(wo, buf.as_mut_ptr(), 4).build();
        unsafe {
            ring.submission().push(&w)?;
            ring.submission().push(&r)?;
        }
        ring.submit()?;
        for _ in 0..2 {
            assert_eq!(drain_one(&mut ring).await.result(), -9);
        }
        assert_eq!(buf, [0; 4]);
        let reader = tokio_fs::File::from_std(file);
        let start = tokio::time::Instant::now();
        reader.read_at(&mut buf, 0).await?;
        assert!(start.elapsed() >= latency);
        assert_eq!(&buf, b"data");
        Ok(())
    });
    sim.run()
}

/// A ring write through a descriptor that appends lands at the end of the
/// file, wherever it is aimed, and that is the page it leaves in the cache.
#[test]
fn ring_write_on_an_append_fd_caches_the_page_it_lands_on() -> Result {
    use std::time::Duration;
    use turmoil::fs::shim::std::fs::{write, File};
    use turmoil::fs::shim::tokio::fs as tokio_fs;

    let mut builder = Builder::new();
    let latency = Duration::from_millis(10);
    let timing = builder.fs().io_latency();
    timing.min_latency(latency).max_latency(latency);
    builder.fs().page_cache();
    let mut sim = builder.build();
    sim.client("c", async move {
        const END: u64 = 3 * 4096;
        write("/log", vec![0; END as usize])?;
        let log = OpenOptions::new().append(true).open("/log")?;
        let mut ring = IoUring::new(4)?;
        let w = opcode::Write::new(types::Fd(log.as_raw_fd()), b"tail".as_ptr(), 4);
        unsafe { ring.submission().push(&w.build())? };
        ring.submit()?;
        assert_eq!(drain_one(&mut ring).await.result(), 4);

        let reader = tokio_fs::File::from_std(File::open("/log")?);
        let mut buf = [0; 4];
        let start = tokio::time::Instant::now();
        reader.read_at(&mut buf, END).await?;
        let (hit, start) = (start.elapsed(), tokio::time::Instant::now());
        reader.read_at(&mut buf, 0).await?;
        assert!(hit < latency && start.elapsed() >= latency, "{hit:?}");
        Ok(())
    });
    sim.run()
}

/// A corrupted read is reported to the hook whichever way it was made.
#[test]
fn ring_read_reports_corruption() -> std::io::Result<()> {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use turmoil::fs::shim::std::fs::{metadata, write, File};
    use turmoil::fs::{FsConfig, FsCorruption, FsState};
    use turmoil::io_uring::host::{enter, EnterCtx, IoUringHostState};

    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut config = FsConfig::default();
    config.corruption_probability(1.0);
    let fs = Arc::new(Mutex::new(FsState::new(config, 0)));
    let rings = Arc::new(Mutex::new(IoUringHostState::new()));
    let now = Duration::ZERO;
    let _fs = turmoil::fs::enter(&fs);
    let _rings = enter(&rings, EnterCtx { now });
    let other = IoUring::new(4)?;
    let other_fd = other.as_raw_fd();
    let (owner, registry) = (Arc::downgrade(&fs), Arc::downgrade(&rings));
    let record = move |event: &FsCorruption| {
        // Fail promptly if re-entering either subsystem would deadlock.
        assert!(owner.upgrade().unwrap().try_lock().is_ok());
        assert!(registry.upgrade().unwrap().try_lock().is_ok());
        let _watch = AsyncFd::new(RingFdHandle(other_fd)).unwrap();
        assert_eq!(metadata(&event.path).unwrap().len(), 4);
        seen.lock().unwrap().push(event.clone());
    };
    fs.lock().unwrap().on_corruption = Some(Arc::new(record));

    write("/file", b"data")?;
    let file = File::open("/file")?;
    let mut ring = IoUring::new(4)?;
    let mut buf = [0u8; 4];
    let fd = types::Fd(file.as_raw_fd());
    let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32).build();
    unsafe {
        ring.submission().push(&r).expect("push r");
    }
    ring.submit()?;
    let mut completions = ring.completion();
    completions.sync();
    assert_eq!(completions.next().map(|cqe| cqe.result()), Some(4));
    assert_ne!(&buf, b"data");

    let events = events.lock().unwrap();
    let [event] = events.as_slice() else {
        panic!("expected one corruption, got {events:?}");
    };
    assert_eq!(event.path, Path::new("/file"));
    assert!(event.offset < 4 && event.len == 1);
    Ok(())
}

/// Software that submits to its ring while it is torn down fails on its
/// own, without taking the simulation down with it.
#[test]
fn bounce_survives_a_destructor_that_submits() -> Result {
    use turmoil::fs::shim::std::fs::File;

    struct Engine {
        ring: IoUring,
        _file: File,
    }

    impl Drop for Engine {
        fn drop(&mut self) {
            let _ = self.ring.submit();
        }
    }

    restart(
        Builder::new().build(),
        false,
        || {
            let (ring, _file) = (IoUring::new(4)?, open_rw("/data")?);
            Ok(Engine { ring, _file })
        },
        || Ok(()),
    )
}

/// A ring goes away with the software that made it, even one it leaked.
#[test_case(false; "dropped")]
#[test_case(true; "leaked")]
fn bounce_closes_rings(leak: bool) -> Result {
    use std::sync::atomic::{AtomicI32, Ordering};

    static RING: AtomicI32 = AtomicI32::new(0);

    restart(
        Builder::new().build(),
        false,
        move || {
            let ring = IoUring::new(4)?;
            RING.store(ring.as_raw_fd(), Ordering::SeqCst);
            if leak {
                std::mem::forget(ring);
                return Ok(None);
            }
            Ok(Some(ring))
        },
        || {
            let stale = RingFdHandle(RING.load(Ordering::SeqCst));
            assert!(AsyncFd::new(stale).is_err(), "ring outlived the bounce");
            Ok(())
        },
    )
}

/// A ring syncs a directory through a descriptor on it, and refuses to
/// read it.
#[test]
fn ring_fsync_on_a_directory_makes_its_entries_durable() -> Result {
    use turmoil::fs::shim::std::fs::{read, sync_dir, write, File};

    restart(
        Builder::new().build(),
        true,
        || {
            create_dir_all(TEST_DIR)?;
            sync_dir("/")?;
            write(format!("{TEST_DIR}/named"), b"")?;
            let dir = File::open(TEST_DIR)?;
            let fd = types::Fd(dir.as_raw_fd());
            let mut ring = IoUring::new(4)?;

            let mut buf = [0u8; 4];
            let read = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32);
            let fsync = opcode::Fsync::new(fd);
            for entry in [read.build().user_data(1), fsync.build().user_data(2)] {
                unsafe { ring.submission().push(&entry).expect("push") };
            }
            ring.submit()?;
            let mut completed = ring.completion();
            completed.sync();
            let result = |cqe: cqueue::Entry| (cqe.user_data(), cqe.result());
            let mut results: Vec<_> = completed.map(result).collect();
            results.sort_unstable();
            assert_eq!(results, [(1, -21), (2, 0)], "expected -EISDIR and success");
            Ok(())
        },
        || {
            assert_eq!(read(format!("{TEST_DIR}/named"))?, b"");
            Ok(())
        },
    )
}

/// Local newtype: AsyncFd::new is generic over `T: AsRawFd`, and we
/// want to drive it directly from the ring's fd integer rather than
/// borrowing the IoUring (which would tie up `&mut ring` while we
/// also need to push more SQEs).
struct RingFdHandle(std::os::fd::RawFd);
impl std::os::fd::AsRawFd for RingFdHandle {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0
    }
}

/// Drain one CQE, advancing simulated time as needed.
///
/// Awaits the ring's `AsyncFd::readable` so we don't spin: simulated
/// time auto-advances to the next scheduled completion deadline.
async fn drain_one(ring: &mut IoUring) -> cqueue::Entry {
    let async_fd = AsyncFd::new(RingFdHandle(<IoUring as std::os::fd::AsRawFd>::as_raw_fd(
        ring,
    )))
    .expect("AsyncFd");
    loop {
        // Scope the cq handle so its borrow of `ring` ends before we
        // await on async_fd (the readable future doesn't need the
        // ring, but the borrow checker won't let us hold both).
        let cqe = {
            let mut cq = ring.completion();
            cq.sync();
            cq.next()
        };
        if let Some(cqe) = cqe {
            return cqe;
        }
        let _ = async_fd.readable().await.expect("readable");
    }
}
