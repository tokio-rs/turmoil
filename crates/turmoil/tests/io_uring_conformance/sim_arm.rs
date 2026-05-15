//! Conformance tests, sim backend (turmoil::io_uring).
//!
//! These exercise read / write / fsync against the simulated ring
//! and AsyncFd. The same test bodies are mirrored byte-for-byte by
//! `real_arm.rs`, except for the per-backend fixture (file open and
//! ring construction).

use std::os::fd::AsRawFd;
use turmoil::fs::shim::std::fs::{create_dir_all, OpenOptions};
use turmoil::io_uring::{opcode, types, AsyncFd, IoUring};
use turmoil::{Builder, Result};

#[test]
fn read_returns_full_buffer() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/conformance/read_full")?;
        let payload = b"abcdefghij".to_vec();

        let mut ring = IoUring::new(4).expect("ring");
        let fd = types::Fd(file.as_raw_fd());

        // Seed.
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).unwrap();
        }
        ring.submit().unwrap();
        let _ = drain_one(&mut ring).await;

        // Read full buffer.
        let mut buf = vec![0u8; payload.len()];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(2);
        unsafe {
            ring.submission().push(&r).unwrap();
        }
        ring.submit().unwrap();
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 2);
        assert_eq!(cqe.result(), payload.len() as i32);
        assert_eq!(buf, payload);
        Ok(())
    });
    sim.run()
}

#[test]
fn write_then_fsync_completes() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/conformance/sync")?;
        let mut ring = IoUring::new(4).expect("ring");
        let fd = types::Fd(file.as_raw_fd());

        let payload = b"sync me".to_vec();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&w).unwrap();
        }

        let f = opcode::Fsync::new(fd).build().user_data(2);
        unsafe {
            ring.submission().push(&f).unwrap();
        }
        ring.submit().unwrap();

        let mut by_ud = std::collections::HashMap::new();
        for _ in 0..2 {
            let cqe = drain_one(&mut ring).await;
            by_ud.insert(cqe.user_data(), cqe.result());
        }
        assert_eq!(by_ud.get(&1).copied(), Some(payload.len() as i32));
        assert_eq!(by_ud.get(&2).copied(), Some(0));
        Ok(())
    });
    sim.run()
}

#[test]
fn read_with_bad_fd_returns_ebadf() -> Result {
    // Both backends must surface a closed/invalid fd as -EBADF on the
    // CQE result.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let mut ring = IoUring::new(4).expect("ring");
        let mut buf = [0u8; 4];
        let bogus = types::Fd(-1);
        let r = opcode::Read::new(bogus, buf.as_mut_ptr(), buf.len() as u32)
            .build()
            .user_data(1);
        unsafe {
            ring.submission().push(&r).unwrap();
        }
        ring.submit().unwrap();
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), -9, "expected -EBADF");
        Ok(())
    });
    sim.run()
}

#[test]
fn cancel_unknown_user_data_returns_enoent() -> Result {
    // Both backends report -ENOENT for a cancel against a user_data
    // that has no in-flight match.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        let mut ring = IoUring::new(4).expect("ring");
        let c = opcode::AsyncCancel::new(0xdead_beef).build().user_data(1);
        unsafe {
            ring.submission().push(&c).unwrap();
        }
        ring.submit().unwrap();
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.user_data(), 1);
        assert_eq!(cqe.result(), -2, "expected -ENOENT");
        Ok(())
    });
    sim.run()
}

#[test]
fn drain_n_cqes_all_accounted_for() -> Result {
    // Both backends must deliver exactly N CQEs for N submitted ops,
    // each with the correct user_data and (here) result. Order is
    // not asserted — kernels may shuffle, sim does shuffle.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/conformance/multi")?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(16).expect("ring");

        // Seed 8 distinct bytes at 8 distinct offsets.
        let payload: Vec<u8> = (0..8).collect();
        let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
            .build()
            .user_data(0);
        unsafe {
            ring.submission().push(&w).unwrap();
        }
        ring.submit().unwrap();
        let _ = drain_one(&mut ring).await;

        // Submit 8 reads, each for a single byte.
        let mut bufs: Vec<[u8; 1]> = vec![[0u8; 1]; 8];
        for (i, b) in bufs.iter_mut().enumerate() {
            let r = opcode::Read::new(fd, b.as_mut_ptr(), 1)
                .offset(i as u64)
                .build()
                .user_data(100 + i as u64);
            unsafe {
                ring.submission().push(&r).unwrap();
            }
        }
        ring.submit().unwrap();

        // Drain all 8 and verify each one's result + buffer.
        let mut seen_uds: Vec<u64> = Vec::new();
        for _ in 0..8 {
            let cqe = drain_one(&mut ring).await;
            assert_eq!(cqe.result(), 1, "each read should report 1 byte");
            seen_uds.push(cqe.user_data());
        }
        seen_uds.sort_unstable();
        assert_eq!(seen_uds, (100..108).collect::<Vec<_>>());

        // Each buffer should contain its index byte.
        for (i, b) in bufs.iter().enumerate() {
            assert_eq!(b[0], i as u8, "buf[{i}] = {} expected {i}", b[0]);
        }
        Ok(())
    });
    sim.run()
}

struct RingFdHandle(std::os::fd::RawFd);
impl std::os::fd::AsRawFd for RingFdHandle {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0
    }
}

async fn drain_one(ring: &mut IoUring) -> turmoil::io_uring::cqueue::Entry {
    let async_fd =
        AsyncFd::new(RingFdHandle(<IoUring as AsRawFd>::as_raw_fd(ring))).expect("AsyncFd");
    loop {
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
