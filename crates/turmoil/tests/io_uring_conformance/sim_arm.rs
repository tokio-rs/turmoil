//! Conformance tests, sim backend (turmoil::io_uring).
//!
//! These exercise read / write / fsync against the simulated ring
//! and AsyncFd. The same test bodies are mirrored byte-for-byte by
//! `real_arm.rs`, except for the per-backend fixture (file open and
//! ring construction).

use std::os::fd::AsRawFd;
use turmoil::fs::shim::std::fs::{create_dir_all, read, write, File, OpenOptions};
use turmoil::io_uring::{opcode, squeue, types, AsyncFd, IoUring};
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
fn directory_fd_fails_reads_and_fsyncs() -> Result {
    // Both backends open a directory read-only. Reading it fails with
    // -EISDIR, and fsync on it completes.
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance/dir")?;
        let dir = File::open("/conformance/dir")?;
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
        Ok(())
    });
    sim.run()
}

/// Submits `entries` and returns their results in the order of their
/// `user_data`, which counts up from one.
async fn results(ring: &mut IoUring, entries: &[squeue::Entry]) -> Vec<i32> {
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

#[test]
fn append_fd_writes_at_the_end() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/log", b"0123456789")?;
        let log = OpenOptions::new().append(true).open("/conformance/log")?;
        let fd = types::Fd(log.as_raw_fd());
        let mut ring = IoUring::new(4).expect("ring");

        let w = opcode::Write::new(fd, b"ab".as_ptr(), 2).offset(2);
        assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [2]);
        assert_eq!(read("/conformance/log")?, b"0123456789ab");
        Ok(())
    });
    sim.run()
}

#[test]
fn transfers_the_fd_is_not_open_for_return_ebadf() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", b"0123456789")?;
        let reader = File::open("/conformance/file")?;
        let writer = OpenOptions::new().write(true).open("/conformance/file")?;
        let (ro, wo) = (types::Fd(reader.as_raw_fd()), types::Fd(writer.as_raw_fd()));
        let mut ring = IoUring::new(4).expect("ring");

        let mut buf = [0u8; 4];
        let w = opcode::Write::new(ro, b"ab".as_ptr(), 2);
        let r = opcode::Read::new(wo, buf.as_mut_ptr(), 4);
        let entries = [w.build().user_data(1), r.build().user_data(2)];
        let refused = results(&mut ring, &entries).await;
        assert_eq!(refused, [-9, -9], "expected -EBADF");
        assert_eq!(read("/conformance/file")?, b"0123456789");
        Ok(())
    });
    sim.run()
}

#[test]
fn offset_minus_one_is_the_file_position() -> Result {
    use std::io::{Seek, SeekFrom};

    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", b"0123456789")?;
        let mut options = OpenOptions::new();
        let mut file = options.read(true).write(true).open("/conformance/file")?;
        file.seek(SeekFrom::Start(4))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("ring");

        let mut buf = [0u8; 2];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), 2).offset(u64::MAX);
        assert_eq!(results(&mut ring, &[r.build().user_data(1)]).await, [2]);
        assert_eq!(&buf, b"45");
        let w = opcode::Write::new(fd, b"ab".as_ptr(), 2).offset(u64::MAX);
        assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [2]);
        assert_eq!(file.stream_position()?, 8);
        assert_eq!(read("/conformance/file")?, b"012345ab89");
        Ok(())
    });
    sim.run()
}

#[test]
fn position_is_taken_when_a_transfer_is_submitted() -> Result {
    use std::io::{Seek, SeekFrom};

    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", b"0123456789")?;
        let mut file = File::open("/conformance/file")?;
        file.seek(SeekFrom::Start(4))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("ring");

        let mut buf = [0u8; 2];
        let r = opcode::Read::new(fd, buf.as_mut_ptr(), 2).offset(u64::MAX);
        unsafe { ring.submission().push(&r.build().user_data(1)).unwrap() };
        ring.submit().unwrap();
        file.seek(SeekFrom::Start(0))?;
        assert_eq!(drain_one(&mut ring).await.result(), 2);
        // The simulated disk is one that the kernel reads without blocking.
        assert_eq!(&buf, b"45");
        Ok(())
    });
    sim.run()
}

#[test]
fn direct_io_at_the_file_position_may_leave_it() -> Result {
    use std::alloc::{alloc_zeroed, dealloc, Layout};
    use std::io::{Seek, SeekFrom};

    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", vec![b'0'; 4 * 4096])?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).direct_io(true);
        let mut file = options.open("/conformance/file")?;
        file.seek(SeekFrom::Start(4096))?;
        let fd = types::Fd(file.as_raw_fd());
        let mut ring = IoUring::new(4).expect("ring");
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        let page = unsafe { alloc_zeroed(layout) };

        let mut positions = Vec::new();
        for byte in *b"AB" {
            unsafe { std::slice::from_raw_parts_mut(page, 4096).fill(byte) };
            let w = opcode::Write::new(fd, page, 4096).offset(u64::MAX);
            assert_eq!(results(&mut ring, &[w.build().user_data(1)]).await, [4096]);
            positions.push(file.stream_position()?);
        }
        let r = opcode::Read::new(fd, page, 4096).offset(u64::MAX);
        assert_eq!(results(&mut ring, &[r.build().user_data(1)]).await, [4096]);
        positions.push(file.stream_position()?);
        unsafe { dealloc(page, layout) };

        let content = read("/conformance/file")?;
        let pages: Vec<u8> = content.chunks(4096).map(|page| page[0]).collect();
        // The simulated disk completes direct I/O later, every time.
        assert_eq!((positions, pages), (vec![4096; 3], b"0B00".to_vec()));
        Ok(())
    });
    sim.run()
}

#[test]
fn empty_transfers_call_on_no_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", b"0123456789")?;
        let dir = File::open("/conformance")?;
        let reader = File::open("/conformance/file")?;
        let mut options = OpenOptions::new();
        let writer = options.write(true).open("/conformance/file")?;
        let fd = |file: &File| types::Fd(file.as_raw_fd());
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
        Ok(())
    });
    sim.run()
}

#[test]
fn transfers_past_the_last_offset_are_invalid() -> Result {
    const LAST: u64 = i64::MAX as u64;

    let mut sim = Builder::new().build();
    sim.client("c", async {
        create_dir_all("/conformance")?;
        write("/conformance/file", b"0123456789")?;
        let dir = File::open("/conformance")?;
        let mut options = OpenOptions::new();
        let file = options.read(true).write(true).open("/conformance/file")?;
        let log = options.append(true).open("/conformance/file")?;
        let fd = |file: &File| types::Fd(file.as_raw_fd());
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
        assert_eq!(read("/conformance/file")?, b"0123456789");
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
