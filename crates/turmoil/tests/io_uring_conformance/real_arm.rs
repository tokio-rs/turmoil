//! Conformance tests, real backend (Linux io_uring + tokio AsyncFd).
//!
//! Mirror image of `sim_arm.rs`. The bodies are deliberately the same
//! shape so a future refactor could collapse them into a macro; today
//! the two backends differ enough at the fixture level (file open,
//! ring construction, AsyncFd type) that explicit duplication is
//! easier to read than a heavily cfg'd macro.

use io_uring::{opcode, types, IoUring};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::io::unix::AsyncFd;

fn tmp_file(dir: &TempDir, name: &str) -> std::fs::File {
    let path: PathBuf = dir.path().join(name);
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .expect("open scratch file")
}

#[tokio::test(flavor = "current_thread")]
async fn read_returns_full_buffer() {
    let dir = TempDir::new().unwrap();
    let file = tmp_file(&dir, "read_full");
    let payload = b"abcdefghij".to_vec();

    let mut ring = IoUring::new(4).expect("ring");
    let fd = types::Fd(file.as_raw_fd());

    let w = opcode::Write::new(fd, payload.as_ptr(), payload.len() as u32)
        .build()
        .user_data(1);
    unsafe {
        ring.submission().push(&w).unwrap();
    }
    ring.submit().unwrap();
    let _ = drain_one(&mut ring).await;

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
}

#[tokio::test(flavor = "current_thread")]
async fn write_then_fsync_completes() {
    let dir = TempDir::new().unwrap();
    let file = tmp_file(&dir, "sync");

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
}

#[tokio::test(flavor = "current_thread")]
async fn read_with_bad_fd_returns_ebadf() {
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
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_unknown_user_data_returns_enoent() {
    let mut ring = IoUring::new(4).expect("ring");
    let c = opcode::AsyncCancel::new(0xdead_beef).build().user_data(1);
    unsafe {
        ring.submission().push(&c).unwrap();
    }
    ring.submit().unwrap();
    let cqe = drain_one(&mut ring).await;
    assert_eq!(cqe.user_data(), 1);
    assert_eq!(cqe.result(), -2, "expected -ENOENT");
}

#[tokio::test(flavor = "current_thread")]
async fn drain_n_cqes_all_accounted_for() {
    let dir = TempDir::new().unwrap();
    let file = tmp_file(&dir, "multi");
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

    let mut seen_uds: Vec<u64> = Vec::new();
    for _ in 0..8 {
        let cqe = drain_one(&mut ring).await;
        assert_eq!(cqe.result(), 1);
        seen_uds.push(cqe.user_data());
    }
    seen_uds.sort_unstable();
    assert_eq!(seen_uds, (100..108).collect::<Vec<_>>());

    for (i, b) in bufs.iter().enumerate() {
        assert_eq!(b[0], i as u8, "buf[{i}] = {} expected {i}", b[0]);
    }
}

struct RingFdHandle(std::os::fd::RawFd);
impl std::os::fd::AsRawFd for RingFdHandle {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0
    }
}

async fn drain_one(ring: &mut IoUring) -> io_uring::cqueue::Entry {
    let async_fd = AsyncFd::with_interest(
        RingFdHandle(ring.as_raw_fd()),
        tokio::io::Interest::READABLE,
    )
    .expect("AsyncFd");
    loop {
        // SAFETY: single consumer of CQ.
        let mut cq = unsafe { ring.completion_shared() };
        cq.sync();
        if let Some(cqe) = cq.next() {
            return cqe;
        }
        drop(cq);
        let mut guard = async_fd.readable().await.expect("readable");
        guard.clear_ready();
    }
}
