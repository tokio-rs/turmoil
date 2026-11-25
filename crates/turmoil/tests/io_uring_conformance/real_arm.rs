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
async fn directory_fd_fails_reads_and_fsyncs() {
    let dir = TempDir::new().unwrap();
    let dir = std::fs::File::open(dir.path()).expect("open scratch dir");
    let fd = types::Fd(dir.as_raw_fd());
    let mut ring = IoUring::new(4).expect("ring");

    let mut buf = [0u8; 4];
    let r = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
        .build()
        .user_data(1);
    let f = opcode::Fsync::new(fd).build().user_data(2);
    unsafe {
        ring.submission().push(&r).unwrap();
        ring.submission().push(&f).unwrap();
    }
    ring.submit().unwrap();

    let mut by_ud = std::collections::HashMap::new();
    for _ in 0..2 {
        let cqe = drain_one(&mut ring).await;
        by_ud.insert(cqe.user_data(), cqe.result());
    }
    assert_eq!(by_ud.get(&1).copied(), Some(-21), "expected -EISDIR");
    assert_eq!(by_ud.get(&2).copied(), Some(0));
}

/// Submits `entries` and returns their results in the order of their
/// `user_data`, which counts up from one.
async fn results(ring: &mut IoUring, entries: &[io_uring::squeue::Entry]) -> Vec<i32> {
    for entry in entries {
        unsafe { ring.submission().push(entry).unwrap() };
    }
    ring.submit().unwrap();
    let mut results = vec![0; entries.len()];
    for _ in entries {
        let cqe = drain_one(ring).await;
        results[cqe.user_data() as usize - 1] = cqe.result();
    }
    results
}

#[tokio::test(flavor = "current_thread")]
async fn append_fd_writes_at_the_end() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("log");
    std::fs::write(&path, b"0123456789").unwrap();
    let mut options = std::fs::OpenOptions::new();
    let log = options.append(true).open(&path).unwrap();
    let fd = types::Fd(log.as_raw_fd());
    let mut ring = IoUring::new(4).expect("ring");

    let w = opcode::Write::new(fd, b"ab".as_ptr(), 2).offset(2);
    assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [2]);
    assert_eq!(std::fs::read(&path).unwrap(), b"0123456789ab");
}

#[tokio::test(flavor = "current_thread")]
async fn transfers_the_fd_is_not_open_for_return_ebadf() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    std::fs::write(&path, b"0123456789").unwrap();
    let reader = std::fs::File::open(&path).unwrap();
    let mut options = std::fs::OpenOptions::new();
    let writer = options.write(true).open(&path).unwrap();
    let (ro, wo) = (types::Fd(reader.as_raw_fd()), types::Fd(writer.as_raw_fd()));
    let mut ring = IoUring::new(4).expect("ring");

    let mut buf = [0u8; 4];
    let w = opcode::Write::new(ro, b"ab".as_ptr(), 2);
    let r = opcode::Read::new(wo, buf.as_mut_ptr(), 4);
    let entries = [w.build().user_data(1), r.build().user_data(2)];
    let refused = results(&mut ring, &entries).await;
    assert_eq!(refused, [-9, -9], "expected -EBADF");
    assert_eq!(std::fs::read(&path).unwrap(), b"0123456789");
}

#[tokio::test(flavor = "current_thread")]
async fn offset_minus_one_is_the_file_position() {
    use std::io::{Seek, SeekFrom};

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    std::fs::write(&path, b"0123456789").unwrap();
    let mut options = std::fs::OpenOptions::new();
    let mut file = options.read(true).write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(4)).unwrap();
    let fd = types::Fd(file.as_raw_fd());
    let mut ring = IoUring::new(4).expect("ring");

    let mut buf = [0u8; 2];
    let r = opcode::Read::new(fd, buf.as_mut_ptr(), 2).offset(u64::MAX);
    assert_eq!(results(&mut ring, &[r.build().user_data(1)]).await, [2]);
    assert_eq!(&buf, b"45");
    let w = opcode::Write::new(fd, b"ab".as_ptr(), 2).offset(u64::MAX);
    assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [2]);
    assert_eq!(file.stream_position().unwrap(), 8);
    assert_eq!(std::fs::read(&path).unwrap(), b"012345ab89");
}

#[tokio::test(flavor = "current_thread")]
async fn position_is_taken_when_a_transfer_is_submitted() {
    use std::io::{Seek, SeekFrom};

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    std::fs::write(&path, b"0123456789").unwrap();
    let mut file = std::fs::File::open(&path).unwrap();
    file.seek(SeekFrom::Start(4)).unwrap();
    let fd = types::Fd(file.as_raw_fd());
    let mut ring = IoUring::new(4).expect("ring");

    let mut buf = [0u8; 2];
    let r = opcode::Read::new(fd, buf.as_mut_ptr(), 2).offset(u64::MAX);
    unsafe { ring.submission().push(&r.build().user_data(1)).unwrap() };
    ring.submit().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(drain_one(&mut ring).await.result(), 2);
    // A filesystem that the kernel can read without blocking has the
    // position taken at once. One that it cannot, such as tmpfs, has a
    // worker take it, before the seek or after.
    assert!([b"45", b"01"].contains(&&buf), "read {buf:?}");
}

#[tokio::test(flavor = "current_thread")]
async fn direct_io_at_the_file_position_may_leave_it() {
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt;

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("file");
    std::fs::write(&path, vec![b'0'; 4 * 4096]).unwrap();
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).custom_flags(libc::O_DIRECT);
    let mut file = options.open(&path).unwrap();
    file.seek(SeekFrom::Start(4096)).unwrap();
    let fd = types::Fd(file.as_raw_fd());
    let mut ring = IoUring::new(4).expect("ring");
    let layout = Layout::from_size_align(4096, 4096).unwrap();
    let page = unsafe { alloc_zeroed(layout) };

    // Each transfer starts where the position is. The kernel moves the
    // position past a transfer that it completes itself, and leaves it for
    // one that the disk completes later. Which it is depends on the
    // filesystem and on the disk, transfer by transfer.
    let moved = |file: &mut std::fs::File, from: u64| {
        let to = file.stream_position().unwrap();
        assert!([from, from + 4096].contains(&to), "from {from} to {to}");
        to
    };
    let (mut position, mut expected) = (4096, b"0000".to_vec());
    for byte in *b"AB" {
        unsafe { std::slice::from_raw_parts_mut(page, 4096).fill(byte) };
        let w = opcode::Write::new(fd, page, 4096).offset(u64::MAX);
        assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [4096]);
        expected[position as usize / 4096] = byte;
        position = moved(&mut file, position);
    }
    let r = opcode::Read::new(fd, page, 4096).offset(u64::MAX);
    assert_eq!(results(&mut ring, &[r.build().user_data(1)]).await, [4096]);
    assert_eq!(unsafe { *page }, expected[position as usize / 4096]);
    moved(&mut file, position);
    unsafe { dealloc(page, layout) };

    let content = std::fs::read(&path).unwrap();
    let pages: Vec<u8> = content.chunks(4096).map(|page| page[0]).collect();
    assert_eq!(pages, expected, "after ending at {position}");
}

#[tokio::test(flavor = "current_thread")]
async fn empty_transfers_call_on_no_file() {
    let scratch = TempDir::new().unwrap();
    let path = scratch.path().join("file");
    std::fs::write(&path, b"0123456789").unwrap();
    let dir = std::fs::File::open(scratch.path()).unwrap();
    let reader = std::fs::File::open(&path).unwrap();
    let mut options = std::fs::OpenOptions::new();
    let writer = options.write(true).open(&path).unwrap();
    let fd = |file: &std::fs::File| types::Fd(file.as_raw_fd());
    let mut ring = IoUring::new(8).expect("ring");

    let ptr = [0u8; 1].as_mut_ptr();
    let get = |fd, offset| opcode::Read::new(fd, ptr, 0).offset(offset).build();
    let put = |fd| opcode::Write::new(fd, ptr, 0).build();
    let entries = [
        get(fd(&dir), 0).user_data(1),
        get(fd(&dir), u64::MAX).user_data(2),
        get(fd(&writer), 0).user_data(3),
        put(fd(&reader)).user_data(4),
        put(fd(&writer)).user_data(5),
    ];
    assert_eq!(results(&mut ring, &entries).await, [0, 0, -9, -9, 0]);
}

#[tokio::test(flavor = "current_thread")]
async fn transfers_past_the_last_offset_are_invalid() {
    const LAST: u64 = i64::MAX as u64;

    let scratch = TempDir::new().unwrap();
    let path = scratch.path().join("file");
    std::fs::write(&path, b"0123456789").unwrap();
    let dir = std::fs::File::open(scratch.path()).unwrap();
    let mut options = std::fs::OpenOptions::new();
    let file = options.read(true).write(true).open(&path).unwrap();
    let log = options.append(true).open(&path).unwrap();
    let fd = |file: &std::fs::File| types::Fd(file.as_raw_fd());
    let mut ring = IoUring::new(8).expect("ring");

    let ptr = [0u8; 4].as_mut_ptr();
    let get = |fd, offset| opcode::Read::new(fd, ptr, 4).offset(offset).build();
    let put = |fd, len, offset| opcode::Write::new(fd, ptr, len).offset(offset).build();
    let entries = [
        get(fd(&file), LAST - 3).user_data(1),
        get(fd(&file), LAST - 4).user_data(2),
        get(fd(&dir), LAST - 3).user_data(3),
        put(fd(&file), 1, LAST).user_data(4),
        put(fd(&log), 2, LAST - 1).user_data(5),
    ];
    assert_eq!(results(&mut ring, &entries).await, [-22, 0, -22, -22, -22]);
    assert_eq!(std::fs::read(&path).unwrap(), b"0123456789");
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
