//! Crash consistency and durability tests.

use std::io::{self, BufWriter, ErrorKind, Read, Write};
use std::mem;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use turmoil::fs::shim::std::fs::{
    create_dir, create_dir_all, exists, hard_link, metadata, read, remove_dir, remove_dir_all,
    remove_file, rename, set_permissions, sync_dir, write, File, OpenOptions, Permissions,
};
use turmoil::fs::shim::std::os::unix::fs::symlink;
use turmoil::fs::{Fs, FsCorruption};
use turmoil::{Builder, Result, Sim};

const TEST_PATH: &str = "/test/data.db";

/// Runs `before` on a host, restarts the host's software, then runs `after`
/// on it. With `crash` the restart is a crash rather than a clean bounce.
/// Whatever `before` returns stays alive until the restart.
pub(super) fn restart<T: 'static>(
    mut sim: Sim<'_>,
    crash: bool,
    before: impl Fn() -> io::Result<T> + Copy + 'static,
    after: impl Fn() -> io::Result<()> + Copy + 'static,
) -> Result {
    fn wait(sim: &mut Sim<'_>, client: &str, done: &Arc<Notify>) -> Result {
        let done = done.clone();
        sim.client(client, async move {
            done.notified().await;
            Ok(())
        });
        sim.run()
    }

    let restarted = Arc::new(AtomicBool::new(false));
    let done = Arc::new(Notify::new());
    let (phase, notify) = (restarted.clone(), done.clone());
    sim.host("server", move || {
        let (phase, notify) = (phase.clone(), notify.clone());
        async move {
            let _held = match phase.load(Ordering::SeqCst) {
                false => Some(before()?),
                true => after().map(|()| None)?,
            };
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });

    wait(&mut sim, "before", &done)?;
    if crash {
        sim.crash("server");
    }
    restarted.store(true, Ordering::SeqCst);
    sim.bounce("server");
    wait(&mut sim, "after", &done)
}

/// Runs `before` on a host, crashes the host, then runs `after` on it.
fn crash(
    before: impl Fn() -> io::Result<()> + Copy + 'static,
    after: impl Fn() -> io::Result<()> + Copy + 'static,
) -> Result {
    restart(Builder::new().build(), true, before, after)
}

/// Writes a file and syncs its data. Its name is durable only once the
/// parent directory is synced too.
fn write_synced(path: &str, data: &[u8]) -> io::Result<()> {
    let file = File::create(path)?;
    file.write_all_at(data, 0)?;
    file.sync_all()
}

#[test]
fn durability_with_sync() -> Result {
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
                create_dir_all("/test")?;
                sync_dir("/")?;
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(TEST_PATH)?;
                file.write_all_at(b"synced data", 0)?;
                file.sync_all()?;
                sync_dir("/test")?;
            } else {
                let file = OpenOptions::new().read(true).open(TEST_PATH)?;
                let mut buf = [0u8; 11];
                file.read_at(&mut buf, 0)?;
                assert_eq!(&buf, b"synced data");
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
fn data_loss_without_sync() -> Result {
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
                create_dir_all("/test")?;
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(TEST_PATH)?;
                file.write_all_at(b"unsynced data", 0)?;
            } else {
                create_dir_all("/test")?;
                let result = OpenOptions::new().read(true).open(TEST_PATH);
                assert!(
                    result.is_err(),
                    "file should not exist after crash without sync"
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
fn random_sync_100_percent() -> Result {
    // With 100% sync probability, file DATA is automatically synced.
    // Note: sync_probability only syncs file data, NOT directory entries.
    // We still need explicit sync_dir() for directory entries to survive crash.
    let mut builder = Builder::new();
    builder.fs().sync_probability(1.0);
    let mut sim = builder.build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all("/test")?;
                // Sync directory entries so file survives crash
                sync_dir("/")?;
                sync_dir("/test")?;
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(TEST_PATH)?;
                // Sync the file's directory entry
                sync_dir("/test")?;
                // Write data - will be auto-synced due to 100% sync_probability
                file.write_all_at(b"auto synced", 0)?;
                // No explicit file.sync_all() needed - sync_probability handles it
            } else {
                let file = OpenOptions::new().read(true).open(TEST_PATH)?;
                let mut buf = [0u8; 11];
                file.read_at(&mut buf, 0)?;
                assert_eq!(&buf, b"auto synced");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn sync_data_vs_sync_all() -> Result {
    use std::sync::atomic::AtomicU8;
    // sync_data on new file - file should NOT survive
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            match phase.load(Ordering::SeqCst) {
                0 => {
                    create_dir_all("/data")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open("/data/file.txt")?;
                    file.write_all_at(b"test data", 0)?;
                    file.sync_data()?;
                }
                _ => {
                    create_dir_all("/data")?;
                    assert!(OpenOptions::new()
                        .read(true)
                        .open("/data/file.txt")
                        .is_err());
                }
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(1, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn sync_dir_durability() -> Result {
    use std::sync::atomic::AtomicU8;
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            match phase.load(Ordering::SeqCst) {
                0 => {
                    use turmoil::fs::shim::std::fs::sync_dir;
                    create_dir("/data")?;
                    sync_dir("/")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open("/data/file.txt")?;
                    file.write_all_at(b"durable", 0)?;
                    file.sync_all()?;
                    sync_dir("/data")?;
                }
                _ => {
                    let file = OpenOptions::new()
                        .read(true)
                        .open("/data/file.txt")
                        .expect("should survive");
                    let mut buf = [0u8; 7];
                    file.read_exact_at(&mut buf, 0)?;
                    assert_eq!(&buf, b"durable");
                }
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(1, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn file_lost_without_dir_sync() -> Result {
    use std::sync::atomic::AtomicU8;
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            match phase.load(Ordering::SeqCst) {
                0 => {
                    use turmoil::fs::shim::std::fs::sync_dir;
                    create_dir("/data")?;
                    sync_dir("/")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open("/data/file.txt")?;
                    file.write_all_at(b"data", 0)?;
                    file.sync_all()?;
                    // No sync_dir("/data") - entry not durable!
                }
                _ => {
                    assert!(OpenOptions::new()
                        .read(true)
                        .open("/data/file.txt")
                        .is_err());
                }
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(1, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn disk_capacity_enospc() -> Result {
    let mut builder = Builder::new();
    builder.fs().capacity(100);
    let mut sim = builder.build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(&[b'A'; 50], 0)?;
        file.write_all_at(&[b'B'; 50], 50)?;
        assert!(file.write_all_at(&[b'C'; 1], 100).is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn disk_capacity_multiple_files() -> Result {
    let mut builder = Builder::new();
    builder.fs().capacity(100);
    let mut sim = builder.build();
    sim.client("test", async {
        let f1 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/file1")?;
        let f2 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/file2")?;
        f1.write_all_at(&[b'A'; 60], 0)?;
        f2.write_all_at(&[b'B'; 40], 0)?;
        assert!(f1.write_all_at(&[b'C'; 1], 60).is_err());
        Ok(())
    });
    sim.run()
}

/// Rewriting a file in place needs no more room than the file already has.
#[test]
fn disk_capacity_allows_rewriting_a_synced_file() -> Result {
    let mut builder = Builder::new();
    builder.fs().capacity(100);
    let mut sim = builder.build();
    sim.client("test", async {
        write_synced("/file", &[0; 80])?;
        write("/file", [1; 80])?;
        Ok(())
    });
    sim.run()
}

/// Holes take no space, so a sparse file larger than the disk leaves room
/// for other files until it is written to.
#[test]
fn disk_capacity_does_not_count_holes() -> Result {
    let mut builder = Builder::new();
    builder.fs().capacity(100);
    let mut sim = builder.build();
    sim.client("test", async {
        let big = File::create("/big")?;
        big.set_len(1 << 40)?;
        write("/small", b"x")?;
        big.write_all_at(&[1; 99], 1 << 30)?;
        let full = big.write_all_at(&[1; 1], 1 << 20).unwrap_err();
        assert_eq!(full.kind(), ErrorKind::StorageFull);
        Ok(())
    });
    sim.run()
}

#[test_case(true; "durable bytes")]
#[test_case(false; "pending bytes")]
fn torn_writes_reserve_space_across_a_truncate(synced: bool) -> io::Result<()> {
    let fs = Fs::builder().capacity(4).block_size(4).build();
    let _guard = fs.enter();
    let file = File::create("/file")?;
    sync_dir("/")?;
    file.write_all_at(b"data", 0)?;
    if synced {
        file.sync_data()?;
    }
    file.set_len(0)?;
    let no_room = file.write_at(b"next", 4096).unwrap_err();
    assert_eq!(no_room.kind(), ErrorKind::StorageFull);
    file.write_all_at(b"fits", 0)?;
    fs.crash();

    let file = OpenOptions::new().write(true).open("/file")?;
    file.write_all_at(b"fits", 0)?;
    file.set_len(0)?;
    file.sync_data()?;
    file.write_all_at(b"next", 4096)?;
    Ok(())
}

use test_case::test_case;

#[test_case(0.0, 0.0, 0.05; "no errors")]
#[test_case(0.5, 0.5, 0.15; "50 percent errors")]
#[test_case(1.0, 1.0, 0.05; "always errors")]
fn io_error_probability_statistical(error_prob: f64, expected_rate: f64, tolerance: f64) -> Result {
    const ITERATIONS: usize = 100;
    let mut error_count = 0;
    for seed in 0..ITERATIONS {
        let mut builder = Builder::new();
        builder.rng_seed(seed as u64);
        builder.fs().io_error_probability(error_prob);
        let mut sim = builder.build();
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let r = result.clone();
        sim.client("test", async move {
            create_dir("/data")?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/data/file.txt")?;
            let write_result = file.write_all_at(b"test", 0);
            *r.lock().unwrap() = Some(write_result.is_err());
            Ok(())
        });
        sim.run()?;
        if result.lock().unwrap().unwrap_or(false) {
            error_count += 1;
        }
    }
    let actual_rate = error_count as f64 / ITERATIONS as f64;
    assert!((actual_rate - expected_rate).abs() <= tolerance);
    Ok(())
}

#[test]
fn unsynced_writes_lost_on_crash() -> Result {
    use std::sync::atomic::AtomicU8;
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            match phase.load(Ordering::SeqCst) {
                0 => {
                    use turmoil::fs::shim::std::fs::sync_dir;
                    // Create file and sync it (empty)
                    create_dir("/data")?;
                    sync_dir("/")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open("/data/file.txt")?;
                    file.sync_all()?;
                    sync_dir("/data")?;

                    // Write data but DON'T sync
                    file.write_all_at(b"AAAA", 0)?;
                    file.write_all_at(b"BBBB", 4)?;
                    // Crash happens here - unsynced writes should be lost
                }
                _ => {
                    // After crash, file should exist but be empty (synced state)
                    let file = OpenOptions::new().read(true).open("/data/file.txt")?;
                    let mut buf = [0u8; 8];
                    file.read_at(&mut buf, 0)?;
                    // Unsynced writes should be lost - file should be zeros
                    assert_eq!(&buf, &[0u8; 8], "unsynced writes should be lost on crash");
                }
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(1, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn corruption_100_percent() -> Result {
    let mut builder = Builder::new();
    builder.fs().corruption_probability(1.0);
    let mut sim = builder.build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file.write_all_at(b"original data", 0)?;
        let mut buf = [0u8; 13];
        file.read_at(&mut buf, 0)?;
        // With 100% corruption, data should be different
        assert_ne!(&buf, b"original data", "data should be corrupted");
        Ok(())
    });
    sim.run()
}

#[test]
fn corruption_hook_names_the_file_as_opened() -> io::Result<()> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let record = move |event: &FsCorruption| seen.lock().unwrap().push(event.clone());
    let fs = Fs::builder()
        .corruption_probability(1.0)
        .on_corruption(record);
    let fs = fs.build();
    let _guard = fs.enter();

    create_dir("/data")?;
    symlink("/data", "/link")?;
    write("/link/file", b"data")?;
    let file = File::open("link/file")?;
    rename("/data/file", "/data/renamed")?;
    file.read_at(&mut [0; 4], 0)?;

    let events = events.lock().unwrap();
    let [event] = events.as_slice() else {
        panic!("expected one corruption, got {events:?}");
    };
    assert_eq!(event.path, Path::new("link/file"));
    assert!(event.offset < 4 && event.len == 1);
    Ok(())
}

#[test_case(0.0, 0.0, 0.05; "no corruption")]
#[test_case(0.5, 0.5, 0.15; "50 percent corruption")]
#[test_case(1.0, 1.0, 0.05; "always corruption")]
fn corruption_probability_statistical(prob: f64, expected_rate: f64, tolerance: f64) -> Result {
    const ITERATIONS: usize = 100;
    let mut corruption_count = 0;
    for seed in 0..ITERATIONS {
        let mut builder = Builder::new();
        builder.rng_seed(seed as u64);
        builder.fs().corruption_probability(prob);
        let mut sim = builder.build();
        let result = std::sync::Arc::new(std::sync::Mutex::new(false));
        let r = result.clone();
        sim.client("test", async move {
            create_dir("/data")?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/data/file.txt")?;
            // Write known data
            file.write_all_at(b"test data", 0)?;
            // Read it back
            let mut buf = [0u8; 9];
            file.read_at(&mut buf, 0)?;
            // Check if corruption occurred
            *r.lock().unwrap() = &buf != b"test data";
            Ok(())
        });
        sim.run()?;
        if *result.lock().unwrap() {
            corruption_count += 1;
        }
    }
    let actual_rate = corruption_count as f64 / ITERATIONS as f64;
    assert!(
        (actual_rate - expected_rate).abs() <= tolerance,
        "expected rate {expected_rate} +/- {tolerance}, got {actual_rate}"
    );
    Ok(())
}

#[test]
fn short_read_100_percent() -> Result {
    let mut builder = Builder::new();
    builder.fs().short_read_probability(1.0);
    let mut sim = builder.build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file.write_all_at(b"0123456789abcdef", 0)?;
        let mut buf = [0u8; 16];
        let n = file.read_at(&mut buf, 0)?;
        assert!((1..16).contains(&n), "short read produced count {n}");
        assert_eq!(&buf[..n], &b"0123456789abcdef"[..n]);
        Ok(())
    });
    sim.run()
}

/// Whatever reads a whole file reads on after a short read, as `std::fs`
/// does: a short read is not the end of the file.
#[test]
fn whole_file_reads_survive_short_reads() -> Result {
    use turmoil::fs::shim::std::fs::{copy, read_to_string};

    let mut builder = Builder::new();
    builder.fs().short_read_probability(1.0);
    let mut sim = builder.build();
    sim.client("test", async {
        write("/file", b"0123456789abcdef")?;
        assert_eq!(read("/file")?, b"0123456789abcdef");
        assert_eq!(read_to_string("/file")?, "0123456789abcdef");
        assert_eq!(copy("/file", "/copy")?, 16);
        let mut copied = String::new();
        File::open("/copy")?.read_to_string(&mut copied)?;
        assert_eq!(copied, "0123456789abcdef");
        Ok(())
    });
    sim.run()
}

#[test]
fn short_read_also_fires_for_direct_io() -> Result {
    // Signals (EINTR) can interrupt any blocking syscall, including aligned
    // O_DIRECT pread, so short reads must also be injected on direct_io files.
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    let mut builder = Builder::new();
    builder.fs().short_read_probability(1.0);
    let mut sim = builder.build();
    sim.client("test", async {
        create_dir("/data")?;
        // Seed the file with 512 bytes via a non-direct handle.
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        writer.write_all_at(&vec![0xabu8; 512], 0)?;
        drop(writer);

        let direct = OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open("/data/file.txt")?;

        unsafe {
            let layout = Layout::from_size_align(512, 512).unwrap();
            let ptr = alloc_zeroed(layout);
            assert!(!ptr.is_null());
            let buf = std::slice::from_raw_parts_mut(ptr, 512);
            let n = direct.read_at(buf, 0)?;
            assert!(
                (1..512).contains(&n),
                "direct_io read should also see a short count, got {n}"
            );
            dealloc(ptr, layout);
        }
        Ok(())
    });
    sim.run()
}

#[test_case(0.0, 0.0, 0.05; "no short reads")]
#[test_case(0.5, 0.5, 0.15; "50 percent short reads")]
#[test_case(1.0, 1.0, 0.05; "always short reads")]
fn short_read_probability_statistical(prob: f64, expected_rate: f64, tolerance: f64) -> Result {
    const ITERATIONS: usize = 100;
    let mut short_count = 0;
    for seed in 0..ITERATIONS {
        let mut builder = Builder::new();
        builder.rng_seed(seed as u64);
        builder.fs().short_read_probability(prob);
        let mut sim = builder.build();
        let result = std::sync::Arc::new(std::sync::Mutex::new(false));
        let r = result.clone();
        sim.client("test", async move {
            create_dir("/data")?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/data/file.txt")?;
            file.write_all_at(b"0123456789abcdef", 0)?;
            let mut buf = [0u8; 16];
            let n = file.read_at(&mut buf, 0)?;
            *r.lock().unwrap() = n < 16;
            Ok(())
        });
        sim.run()?;
        if *result.lock().unwrap() {
            short_count += 1;
        }
    }
    let actual_rate = short_count as f64 / ITERATIONS as f64;
    assert!(
        (actual_rate - expected_rate).abs() <= tolerance,
        "expected rate {expected_rate} +/- {tolerance}, got {actual_rate}"
    );
    Ok(())
}

#[test]
fn torn_write_partial_data_survives() -> Result {
    // With block_size configured, writes may be partially applied on crash.
    // Run many iterations to statistically verify torn writes occur.
    const ITERATIONS: usize = 50;
    let mut partial_write_count = 0;
    let mut full_write_count = 0;
    let mut no_write_count = 0;

    for seed in 0..ITERATIONS {
        let mut builder = Builder::new();
        builder.rng_seed(seed as u64);
        builder.fs().block_size(4); // Small block size for testing
        let mut sim = builder.build();

        let result = Arc::new(std::sync::Mutex::new(Vec::new()));
        let r = result.clone();
        let phase = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let ph = phase.clone();
        let nh = notify.clone();

        sim.host("server", move || {
            let phase = ph.clone();
            let notify = nh.clone();
            let result = r.clone();
            async move {
                if !phase.load(Ordering::SeqCst) {
                    // Setup: create file and sync directory entry
                    create_dir_all("/test")?;
                    sync_dir("/")?;
                    sync_dir("/test")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open(TEST_PATH)?;
                    // Sync empty file and directory entry
                    file.sync_all()?;
                    sync_dir("/test")?;

                    // Write 16 bytes (4 blocks of 4 bytes each) WITHOUT syncing
                    file.write_all_at(b"AAAABBBBCCCCDDDD", 0)?;
                    // Crash happens here - write may be partially applied
                } else {
                    let file = OpenOptions::new().read(true).open(TEST_PATH)?;
                    let mut buf = [0u8; 16];
                    file.read_at(&mut buf, 0)?;
                    *result.lock().unwrap() = buf.to_vec();
                }
                notify.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            }
        });

        let n = notify.clone();
        sim.client("p0", async move {
            n.notified().await;
            Ok(())
        });
        sim.run()?;
        sim.crash("server");
        phase.store(true, Ordering::SeqCst);
        sim.bounce("server");
        let n = notify.clone();
        sim.client("p1", async move {
            n.notified().await;
            Ok(())
        });
        sim.run()?;

        let data = result.lock().unwrap().clone();
        let written_bytes = data.iter().filter(|&&b| b != 0).count();

        if written_bytes == 0 {
            no_write_count += 1;
        } else if written_bytes == 16 {
            full_write_count += 1;
        } else {
            partial_write_count += 1;
        }
    }

    // With torn writes enabled, we should see a mix of outcomes
    // At minimum, we should see some partial writes over many iterations
    assert!(
        partial_write_count > 0 || (no_write_count > 0 && full_write_count > 0),
        "Expected torn writes to produce varied outcomes. \
         Got: partial={partial_write_count}, full={full_write_count}, none={no_write_count}"
    );
    Ok(())
}

#[test]
fn no_torn_writes_without_block_size() -> Result {
    // Without block_size, pending writes should be completely lost on crash
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();

    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            if !phase.load(Ordering::SeqCst) {
                create_dir_all("/test")?;
                sync_dir("/")?;
                sync_dir("/test")?;
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(TEST_PATH)?;
                file.sync_all()?;
                sync_dir("/test")?;
                // Write without sync - should be completely lost
                file.write_all_at(b"AAAABBBBCCCCDDDD", 0)?;
            } else {
                let file = OpenOptions::new().read(true).open(TEST_PATH)?;
                let mut buf = [0u8; 16];
                file.read_at(&mut buf, 0)?;
                // Without block_size, entire write should be lost
                assert_eq!(&buf, &[0u8; 16], "unsynced write should be completely lost");
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });

    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(true, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn torn_write_respects_block_boundaries() -> Result {
    // Verify that torn writes happen at block boundaries
    // With block_size=8, a 16 byte write should either be:
    // - 0 bytes (0 blocks)
    // - 8 bytes (1 block)
    // - 16 bytes (2 blocks)
    const ITERATIONS: usize = 100;
    let mut valid_sizes = std::collections::HashSet::new();

    for seed in 0..ITERATIONS {
        let mut builder = Builder::new();
        builder.rng_seed(seed as u64);
        builder.fs().block_size(8);
        let mut sim = builder.build();

        let result = Arc::new(std::sync::Mutex::new(0usize));
        let r = result.clone();
        let phase = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        let ph = phase.clone();
        let nh = notify.clone();

        sim.host("server", move || {
            let phase = ph.clone();
            let notify = nh.clone();
            let result = r.clone();
            async move {
                if !phase.load(Ordering::SeqCst) {
                    create_dir_all("/test")?;
                    sync_dir("/")?;
                    sync_dir("/test")?;
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .open(TEST_PATH)?;
                    file.sync_all()?;
                    sync_dir("/test")?;
                    file.write_all_at(b"AAAAAAAABBBBBBBB", 0)?;
                } else {
                    let file = OpenOptions::new().read(true).open(TEST_PATH)?;
                    let mut buf = [0u8; 16];
                    file.read_at(&mut buf, 0)?;
                    let written = buf.iter().filter(|&&b| b != 0).count();
                    *result.lock().unwrap() = written;
                }
                notify.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            }
        });

        let n = notify.clone();
        sim.client("p0", async move {
            n.notified().await;
            Ok(())
        });
        sim.run()?;
        sim.crash("server");
        phase.store(true, Ordering::SeqCst);
        sim.bounce("server");
        let n = notify.clone();
        sim.client("p1", async move {
            n.notified().await;
            Ok(())
        });
        sim.run()?;

        valid_sizes.insert(*result.lock().unwrap());
    }

    // All observed sizes should be multiples of block_size (0, 8, or 16)
    for size in &valid_sizes {
        assert!(
            *size == 0 || *size == 8 || *size == 16,
            "Torn write produced non-block-aligned size: {size}"
        );
    }
    Ok(())
}

/// Regression test: sync_file on newly created file should work correctly.
/// This tests that SetLen/Write ops are applied even when CreateFile is still pending.
#[test]
fn sync_new_file_with_truncate() -> Result {
    use std::sync::atomic::AtomicU8;
    let mut sim = Builder::new().build();
    let phase = Arc::new(AtomicU8::new(0));
    let notify = Arc::new(Notify::new());
    let ph = phase.clone();
    let nh = notify.clone();
    sim.host("server", move || {
        let phase = ph.clone();
        let notify = nh.clone();
        async move {
            match phase.load(Ordering::SeqCst) {
                0 => {
                    use turmoil::fs::shim::std::fs::sync_dir;
                    create_dir("/data")?;
                    sync_dir("/")?;

                    // Create file with truncate (common pattern)
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open("/data/file.txt")?;

                    // Write some data
                    file.write_all_at(b"test data", 0)?;

                    // Sync file data - this should work even though CreateFile is pending
                    file.sync_all()?;

                    // Now sync directory to make entry durable
                    sync_dir("/data")?;
                }
                _ => {
                    let file = OpenOptions::new().read(true).open("/data/file.txt")?;
                    let mut buf = [0u8; 9];
                    file.read_exact_at(&mut buf, 0)?;
                    assert_eq!(&buf, b"test data", "data should survive crash");
                }
            }
            notify.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n = notify.clone();
    sim.client("p0", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()?;
    sim.crash("server");
    phase.store(1, Ordering::SeqCst);
    sim.bounce("server");
    let n = notify.clone();
    sim.client("p1", async move {
        n.notified().await;
        Ok(())
    });
    sim.run()
}

/// Test that sync_data on newly created file also works
#[test]
fn sync_data_new_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;

        // Create file
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;

        // Write data
        file.write_all_at(b"hello", 0)?;

        // sync_data should work even though CreateFile is pending
        file.sync_data()?;

        // Verify data is readable
        let mut buf = [0u8; 5];
        file.read_exact_at(&mut buf, 0)?;
        assert_eq!(&buf, b"hello");

        Ok(())
    });
    sim.run()
}

#[test]
fn renamed_dir_keeps_its_contents_after_crash() -> Result {
    crash(
        || {
            create_dir_all("/a/sub")?;
            write_synced("/a/file", b"one")?;
            write_synced("/a/sub/file", b"two")?;
            for dir in ["/", "/a", "/a/sub"] {
                sync_dir(dir)?;
            }
            rename("/a", "/b")?;
            sync_dir("/")
        },
        || {
            assert_eq!(read("/b/file")?, b"one");
            assert_eq!(read("/b/sub/file")?, b"two");
            assert!(!exists("/a")?);
            Ok(())
        },
    )
}

#[test]
fn unsynced_dir_rename_is_undone_by_crash() -> Result {
    crash(
        || {
            create_dir("/a")?;
            write_synced("/a/file", b"one")?;
            sync_dir("/")?;
            sync_dir("/a")?;
            rename("/a", "/b")
        },
        || {
            assert_eq!(read("/a/file")?, b"one");
            assert!(!exists("/b")?);
            Ok(())
        },
    )
}

#[test_case(&["/c/new"], "/a/old"; "only the directory itself is synced")]
#[test_case(&["/a", "/c/new"], "/b/mid"; "the first source was synced")]
#[test_case(&["/b", "/c/new"], "/c/new"; "the intermediate parent was synced")]
#[test_case(&["/c/new", "/c"], "/c/new"; "the destination is synced afterwards")]
fn syncing_a_new_directory_after_renames(
    syncs: &'static [&'static str],
    restored: &'static str,
) -> Result {
    crash(
        move || {
            for parent in ["/a", "/b", "/c"] {
                create_dir(parent)?;
            }
            sync_dir("/")?;
            create_dir("/a/old")?;
            write_synced("/a/old/child", b"data")?;
            rename("/a/old", "/b/mid")?;
            rename("/b/mid", "/c/new")?;
            for path in syncs {
                sync_dir(path)?;
            }
            assert_eq!(read("/c/new/child")?, b"data");
            Ok(())
        },
        move || {
            for path in ["/a/old", "/b/mid", "/c/new"] {
                assert_eq!(exists(path)?, path == restored, "{path}");
            }
            assert_eq!(read(Path::new(restored).join("child"))?, b"data");
            Ok(())
        },
    )
}

#[test_case(false, "/a/old"; "the first rename is pending")]
#[test_case(true, "/b/new"; "the first rename was synced")]
fn syncing_after_an_intermediate_parent_is_replaced(
    synced: bool,
    restored: &'static str,
) -> Result {
    crash(
        move || {
            create_dir("/a")?;
            create_dir("/b")?;
            sync_dir("/")?;
            create_dir("/a/old")?;
            create_dir("/temp")?;
            rename("/a/old", "/temp/mid")?;
            if synced {
                sync_dir("/a")?;
            }
            rename("/temp/mid", "/b/new")?;
            remove_dir("/temp")?;
            create_dir("/temp")?;
            sync_dir("/temp")?;
            sync_dir("/b/new")
        },
        move || {
            for path in ["/a/old", "/temp/mid", "/b/new"] {
                assert_eq!(exists(path)?, path == restored, "{path}");
            }
            Ok(())
        },
    )
}

#[test_case(false; "only the directory itself is synced")]
#[test_case(true; "the parent is synced too")]
fn directory_renamed_back_to_its_creation_name(parent_synced: bool) -> Result {
    crash(
        move || {
            create_dir("/taken")?;
            sync_dir("/")?;
            create_dir("/d")?;
            write_synced("/d/child", b"data")?;
            rename("/d", "/taken")?;
            rename("/taken", "/d")?;
            sync_dir("/d")?;
            if parent_synced {
                sync_dir("/")?;
            }
            Ok(())
        },
        move || {
            assert_eq!(read("/d/child")?, b"data");
            assert_eq!(exists("/taken")?, !parent_synced);
            Ok(())
        },
    )
}

#[test]
fn write_after_rename_survives_crash() -> Result {
    crash(
        || {
            create_dir("/d")?;
            write_synced("/d/old", b"OLD")?;
            sync_dir("/")?;
            sync_dir("/d")?;
            rename("/d/old", "/d/new")?;
            let file = OpenOptions::new().write(true).open("/d/new")?;
            file.write_all_at(b"X", 3)?;
            file.sync_all()?;
            sync_dir("/d")
        },
        || {
            assert_eq!(read("/d/new")?, b"OLDX");
            assert!(!exists("/d/old")?);
            Ok(())
        },
    )
}

/// Renaming a temp file into place and syncing only the directory is the
/// classic way to end up with an empty file after a crash. Until then the
/// data must stay readable.
#[test]
fn rename_without_fsync_keeps_the_name_but_not_the_data() -> Result {
    crash(
        || {
            create_dir("/d")?;
            sync_dir("/")?;
            write("/d/tmp", b"NEW")?;
            rename("/d/tmp", "/d/final")?;
            sync_dir("/d")?;
            assert_eq!(read("/d/final")?, b"NEW");
            Ok(())
        },
        || {
            assert_eq!(read("/d/final")?, b"");
            assert!(!exists("/d/tmp")?);
            Ok(())
        },
    )
}

#[test_case(None, "/A/x", "/B/y"; "neither parent synced")]
#[test_case(Some("/A"), "/B/y", "/A/x"; "source parent synced")]
#[test_case(Some("/B"), "/B/y", "/A/x"; "destination parent synced")]
fn rename_across_dirs_is_atomic_across_crash(
    synced: Option<&'static str>,
    survivor: &'static str,
    gone: &'static str,
) -> Result {
    crash(
        move || {
            create_dir("/A")?;
            create_dir("/B")?;
            write_synced("/A/x", b"DATA")?;
            for dir in ["/", "/A", "/B"] {
                sync_dir(dir)?;
            }
            rename("/A/x", "/B/y")?;
            synced.map_or(Ok(()), sync_dir)
        },
        move || {
            assert_eq!(read(survivor)?, b"DATA");
            assert!(!exists(gone)?);
            Ok(())
        },
    )
}

/// Renames become durable in order, so syncing the last directory of a
/// chain cannot leave the file under an earlier name as well.
#[test_case("/a"; "source synced")]
#[test_case("/b"; "overwrite synced")]
#[test_case("/c"; "destination synced")]
fn chained_renames_leave_one_name_after_crash(synced: &'static str) -> Result {
    crash(
        move || {
            for dir in ["/a", "/b", "/c"] {
                create_dir(dir)?;
            }
            write_synced("/a/x", b"DATA")?;
            write_synced("/b/x", b"overwritten")?;
            for dir in ["/", "/a", "/b", "/c"] {
                sync_dir(dir)?;
            }
            rename("/a/x", "/b/x")?;
            rename("/b/x", "/c/x")?;
            sync_dir(synced)
        },
        || {
            assert_eq!(read("/c/x")?, b"DATA");
            assert!(!exists("/a/x")? && !exists("/b/x")?);
            Ok(())
        },
    )
}

#[test]
fn overwritten_transient_name_passes_on_its_removals() -> Result {
    crash(
        || {
            for dir in ["/a", "/b", "/c"] {
                create_dir(dir)?;
            }
            write_synced("/a/file", b"survives")?;
            write_synced("/b/file", b"overwritten")?;
            for dir in ["/", "/a", "/b"] {
                sync_dir(dir)?;
            }
            write("/temp", b"transient")?;
            rename("/temp", "/b/file")?;
            rename("/a/file", "/b/file")?;
            rename("/b/file", "/c/file")?;
            sync_dir("/c")
        },
        || {
            assert_eq!(read("/c/file")?, b"survives");
            assert!(!exists("/b/file")?);
            Ok(())
        },
    )
}

#[test]
fn sync_follows_a_long_chain_of_occupied_names() -> io::Result<()> {
    const N: u32 = 4096;
    let fs = Fs::default();
    let _guard = fs.enter();
    let name = |i| format!("/slots/{i}");
    create_dir("/slots")?;
    write_synced("/incoming", b"incoming")?;
    for i in 0..N {
        write_synced(&name(i), &i.to_le_bytes())?;
    }
    sync_dir("/")?;
    sync_dir("/slots")?;
    rename(name(0), name(N))?;
    for i in 1..N {
        rename(name(i), name(i - 1))?;
    }
    rename("/incoming", name(N - 1))?;
    sync_dir("/")?;
    fs.crash();
    assert_eq!(read(name(N - 1))?, b"incoming");
    assert_eq!(read(name(N))?, 0_u32.to_le_bytes());
    for i in 1..N {
        assert_eq!(read(name(i - 1))?, i.to_le_bytes());
    }
    Ok(())
}

/// A rename is pending until one of its own directories is synced, however
/// many other directories are.
#[test]
fn sync_dir_leaves_renames_in_other_dirs_pending() -> Result {
    crash(
        || {
            for dir in ["/a", "/c"] {
                create_dir(dir)?;
            }
            write_synced("/a/x", b"X")?;
            write_synced("/c/p", b"P")?;
            for dir in ["/", "/a", "/c"] {
                sync_dir(dir)?;
            }
            rename("/a/x", "/a/y")?;
            rename("/c/p", "/c/q")?;
            sync_dir("/c")
        },
        || {
            assert_eq!(read("/a/x")?, b"X");
            assert_eq!(read("/c/q")?, b"P");
            assert!(!exists("/a/y")? && !exists("/c/p")?);
            Ok(())
        },
    )
}

/// A durable file is never stranded: a rename into a directory that a crash
/// could not reach makes the path to that directory durable with it.
#[test]
fn rename_into_a_new_dir_makes_the_dir_durable() -> Result {
    crash(
        || {
            write_synced("/file", b"data")?;
            sync_dir("/")?;
            create_dir_all("/new/sub")?;
            rename("/file", "/new/sub/file")?;
            sync_dir("/new/sub")
        },
        || {
            assert_eq!(read("/new/sub/file")?, b"data");
            assert!(!exists("/file")?);
            Ok(())
        },
    )
}

/// The path to a name's new directory becomes durable only for a name a
/// crash would have found before. Here it would not, as `/new` never was
/// synced, so the rename leaves `/new` as lost as it was.
#[test_case("/new/a/final"; "within its directory")]
#[test_case("/new/b/file"; "to another directory")]
fn rename_of_a_name_no_crash_would_find_makes_no_dir_durable(to: &'static str) -> Result {
    crash(
        move || {
            create_dir_all("/new/a")?;
            create_dir_all("/new/b")?;
            write_synced("/new/a/file", b"data")?;
            sync_dir("/new/a")?;
            rename("/new/a/file", to)?;
            sync_dir(Path::new(to).parent().unwrap())
        },
        || {
            assert!(!exists("/new")?);
            Ok(())
        },
    )
}

/// A name stays within reach of the directory it was durable in. `/a/b`
/// is durable before anything above it is, so the rename has to leave the
/// file where a crash will find it once `/a` and `/` are synced as well.
#[test_case("/a/b/d/e/file", "/a/b/d/e"; "destination synced")]
#[test_case("/x/y/file", "/a/b"; "source synced")]
fn rename_synced_before_its_ancestors_keeps_the_file(
    to: &'static str,
    synced: &'static str,
) -> Result {
    crash(
        move || {
            create_dir_all("/a/b")?;
            write_synced("/a/b/file", b"data")?;
            sync_dir("/a/b")?;
            create_dir_all(Path::new(to).parent().unwrap())?;
            rename("/a/b/file", to)?;
            sync_dir(synced)?;
            sync_dir("/a")?;
            sync_dir("/")
        },
        move || {
            assert_eq!(read(to)?, b"data");
            assert!(!exists("/a/b/file")?);
            Ok(())
        },
    )
}

/// The new directory takes the name of the file that moves into it, so it
/// can only become durable as the file gives the name up.
#[test]
fn dir_that_takes_the_name_of_the_file_it_holds_is_durable() -> Result {
    crash(
        || {
            create_dir_all("/a/p")?;
            write_synced("/a/p/n", b"data")?;
            sync_dir("/a/p")?;
            rename("/a/p/n", "/a/p/tmp")?;
            create_dir("/a/p/n")?;
            rename("/a/p/tmp", "/a/p/n/file")?;
            sync_dir("/a/p/n")?;
            sync_dir("/a")?;
            sync_dir("/")
        },
        || {
            assert_eq!(read("/a/p/n/file")?, b"data");
            Ok(())
        },
    )
}

/// Syncing a directory makes what changed within it durable, but not the
/// directory's own rename, which belongs to its parent.
#[test]
fn changes_within_a_renamed_dir_leave_the_dir_where_it_was() -> Result {
    crash(
        || {
            create_dir_all("/x/old/inner")?;
            for dir in ["/", "/x", "/x/old"] {
                sync_dir(dir)?;
            }
            rename("/x", "/y")?;
            rename("/y/old", "/y/new")?;
            create_dir("/y/fresh")?;
            sync_dir("/y")
        },
        || {
            assert!(metadata("/x/new/inner")?.is_dir());
            assert!(metadata("/x/fresh")?.is_dir());
            assert!(!exists("/y")? && !exists("/x/old")?);
            Ok(())
        },
    )
}

#[test]
fn moving_a_subtree_leaves_a_reachable_ancestor_rename_pending() -> Result {
    crash(
        || {
            create_dir_all("/P/B")?;
            create_dir_all("/P/D/child")?;
            for dir in ["/", "/P", "/P/D"] {
                sync_dir(dir)?;
            }
            rename("/P", "/Q")?;
            rename("/Q/D", "/Q/B/D")?;
            sync_dir("/Q/B")
        },
        || {
            assert!(metadata("/P/B/D/child")?.is_dir());
            assert!(!exists("/Q")? && !exists("/P/D")?);
            Ok(())
        },
    )
}

/// One sync takes the file out of `/g/p/s` and makes the move of that
/// directory to the root durable. The file then has to be found wherever a
/// crash finds `/s`, which is no longer wherever it finds `/g/p`.
#[test]
fn file_stays_in_reach_of_a_dir_that_moves_in_the_same_sync() -> Result {
    crash(
        || {
            create_dir_all("/g/p/s")?;
            create_dir("/q")?;
            write_synced("/g/p/s/a", b"a")?;
            write_synced("/q/b", b"b")?;
            for dir in ["/g/p/s", "/g/p", "/q"] {
                sync_dir(dir)?;
            }
            rename("/g/p/s", "/s")?;
            rename("/s/a", "/g/p/a")?;
            rename("/q/b", "/s/b")?;
            sync_dir("/s")
        },
        || {
            assert_eq!(read("/s/b")?, b"b");
            assert_eq!(read("/g/p/a")?, b"a");
            Ok(())
        },
    )
}

/// `/a` was durable inside `/p/x/c` before it moved to the root and `x`
/// moved into it. As `x` gets a durable name there, the move of `/a` has to
/// become durable too, or the three would end up inside each other.
#[test]
fn new_dir_does_not_end_up_inside_what_it_holds() -> Result {
    crash(
        || {
            create_dir("/p")?;
            write_synced("/p/x", b"file")?;
            sync_dir("/")?;
            sync_dir("/p")?;
            // The name is durably the file's, so the new `x` gets none.
            rename("/p/x", "/p/y")?;
            create_dir_all("/p/x/c/a")?;
            sync_dir("/p/x/c")?;
            rename("/p/x/c/a", "/a")?;
            rename("/p/x", "/a/x")?;
            sync_dir("/a/x")
        },
        || {
            assert!(metadata("/a/x/c")?.is_dir());
            Ok(())
        },
    )
}

/// The directory the file passed through is gone, so the first rename can
/// only become durable together with the second.
#[test]
fn rename_through_a_removed_dir_lands_where_the_file_lives() -> Result {
    crash(
        || {
            for dir in ["/a", "/c"] {
                create_dir(dir)?;
            }
            write_synced("/a/x", b"data")?;
            for dir in ["/", "/a", "/c"] {
                sync_dir(dir)?;
            }
            create_dir("/tmp")?;
            rename("/a/x", "/tmp/x")?;
            rename("/tmp/x", "/c/x")?;
            remove_dir("/tmp")?;
            sync_dir("/a")
        },
        || {
            assert_eq!(read("/c/x")?, b"data");
            assert!(!exists("/a/x")?);
            Ok(())
        },
    )
}

/// Syncing the removal of a directory makes the renames that emptied it
/// durable too, or its entries would go down with it.
#[test]
fn removing_a_dir_keeps_the_files_renamed_out_of_it() -> Result {
    crash(
        || {
            for dir in ["/old", "/new"] {
                create_dir(dir)?;
            }
            for file in ["/old/a", "/old/b"] {
                write_synced(file, b"data")?;
            }
            for dir in ["/", "/old", "/new"] {
                sync_dir(dir)?;
            }
            rename("/old/a", "/new/a")?;
            rename("/old/b", "/new/b")?;
            remove_dir_all("/old")?;
            sync_dir("/")
        },
        || {
            assert_eq!(read("/new/a")?, b"data");
            assert_eq!(read("/new/b")?, b"data");
            assert!(!exists("/old")?);
            Ok(())
        },
    )
}

/// `/r` replaces the directory that was renamed out from under it, and is
/// then removed along with everything it durably held.
#[test]
fn removing_a_dir_that_replaced_its_own_child_is_durable() -> Result {
    crash(
        || {
            create_dir_all("/r/f/x")?;
            sync_dir("/r/f")?;
            rename("/r/f/x", "/a")?;
            rename("/r", "/a")?;
            sync_dir("/a")?;
            remove_dir_all("/a")?;
            sync_dir("/")
        },
        || {
            assert!(!exists("/a")? && !exists("/r")?);
            Ok(())
        },
    )
}

/// A directory has one parent after a crash, wherever its renames took it
/// between syncs.
#[test]
fn dir_renamed_back_and_forth_has_one_name_after_crash() -> Result {
    crash(
        || {
            create_dir_all("/p/d")?;
            create_dir("/q")?;
            sync_dir("/p/d")?;
            rename("/p/d", "/a")?;
            rename("/a", "/q/a")?;
            remove_dir("/p")?;
            rename("/q/a", "/a")?;
            sync_dir("/a")?;
            sync_dir("/")
        },
        || {
            assert!(metadata("/a")?.is_dir());
            assert!(metadata("/q")?.is_dir());
            assert!(!exists("/q/a")?);
            Ok(())
        },
    )
}

/// `/m` was durable inside `/u/i` and now holds it. Syncing where `/u/i`
/// arrived makes the move of `/m` durable with it, or each would end up
/// inside the other and out of reach.
#[test]
fn dir_moved_under_its_former_child_stays_in_reach() -> Result {
    crash(
        || {
            create_dir_all("/u/i/m")?;
            sync_dir("/u/i")?;
            rename("/u/i/m", "/m")?;
            create_dir("/m/n")?;
            rename("/u/i", "/m/n/i")?;
            sync_dir("/m/n")
        },
        || {
            assert!(metadata("/m/n/i")?.is_dir());
            assert!(!exists("/u")?);
            Ok(())
        },
    )
}

/// The new directory has the name of a file that was renamed there and then
/// removed. Syncing the file's first directory must not bring the file back.
#[test]
fn synced_new_dir_outlives_the_file_that_had_its_name() -> Result {
    crash(
        || {
            for dir in ["/a", "/p"] {
                create_dir(dir)?;
            }
            write_synced("/a/x", b"old")?;
            for dir in ["/", "/a", "/p"] {
                sync_dir(dir)?;
            }
            rename("/a/x", "/p/d")?;
            remove_file("/p/d")?;
            create_dir("/p/d")?;
            write_synced("/p/d/file", b"new")?;
            sync_dir("/p/d")?;
            sync_dir("/a")
        },
        || {
            assert_eq!(read("/p/d/file")?, b"new");
            assert!(!exists("/a/x")?);
            Ok(())
        },
    )
}

/// The new directory never gets a durable name, since its name still
/// belongs to the directory that was renamed away. A file synced in it and
/// then moved out becomes durable with the directory it moved to.
#[test]
fn file_moved_out_of_a_dir_that_was_never_durable_survives() -> Result {
    crash(
        || {
            create_dir("/old")?;
            sync_dir("/")?;
            rename("/old", "/moved")?;
            create_dir("/old")?;
            write_synced("/old/file", b"data")?;
            sync_dir("/old")?;
            rename("/old/file", "/file")?;
            remove_dir("/old")?;
            sync_dir("/")
        },
        || {
            assert_eq!(read("/file")?, b"data");
            assert!(metadata("/moved")?.is_dir());
            assert!(!exists("/old")?);
            Ok(())
        },
    )
}

#[test_case(false, true; "removal not synced")]
#[test_case(true, false; "removal synced")]
fn removal_is_durable_once_the_dir_is_synced(synced: bool, survives: bool) -> Result {
    crash(
        move || {
            write_synced("/file", b"data")?;
            sync_dir("/")?;
            remove_file("/file")?;
            assert!(!exists("/file")?);
            if synced {
                sync_dir("/")?;
            }
            Ok(())
        },
        move || {
            assert_eq!(exists("/file")?, survives);
            Ok(())
        },
    )
}

#[test]
fn chmod_is_durable_once_the_file_is_synced() -> Result {
    crash(
        || {
            let synced = File::create("/synced")?;
            let unsynced = File::create("/unsynced")?;
            let data = File::create("/data")?;
            sync_dir("/")?;
            for path in ["/synced", "/unsynced", "/data"] {
                set_permissions(path, Permissions::from_mode(0o600))?;
            }
            drop(unsynced);
            data.write_all_at(b"x", 0)?;
            data.set_len(3)?;
            data.sync_data()?;
            synced.sync_all()
        },
        || {
            let mode = |path: &str| metadata(path).map(|meta| meta.mode() & 0o7777);
            assert_eq!(mode("/synced")?, 0o600);
            assert_eq!(mode("/unsynced")?, 0o644);
            assert_eq!(mode("/data")?, 0o644);
            assert_eq!(read("/data")?, b"x\0\0");
            Ok(())
        },
    )
}

/// A descriptor opened to sync what it writes makes each write durable by
/// the time it returns: the bytes of the file with `O_DSYNC`, and its
/// attributes as well with `O_SYNC`.
#[test]
fn synchronous_descriptors_make_their_writes_durable() -> Result {
    use std::os::unix::fs::OpenOptionsExt;

    crash(
        || {
            for path in ["/plain", "/data", "/all"] {
                File::create(path)?.sync_all()?;
            }
            sync_dir("/")?;
            let flags = [
                ("/plain", 0),
                ("/data", libc::O_DSYNC),
                ("/all", libc::O_SYNC),
            ];
            for (path, flags) in flags {
                set_permissions(path, Permissions::from_mode(0o600))?;
                let mut options = OpenOptions::new();
                let file = options.write(true).custom_flags(flags).open(path)?;
                file.write_all_at(b"written", 0)?;
            }
            Ok(())
        },
        || {
            let found = |path: &str| {
                let mode = metadata(path)?.mode() & 0o7777;
                io::Result::Ok((read(path)?, mode))
            };
            assert_eq!(found("/plain")?, (vec![], 0o644));
            assert_eq!(found("/data")?, (b"written".to_vec(), 0o644));
            assert_eq!(found("/all")?, (b"written".to_vec(), 0o600));
            Ok(())
        },
    )
}

/// A file replaces another, and then moves into a new directory. A sync
/// of that directory makes durable that the old file is gone, so it has to
/// make the directory durable too: a crash never finds the old file gone
/// with nothing in its place. One sync does all of it.
#[test_case("/n", 0; "into a directory that has the old name, not synced")]
#[test_case("/n", 1; "into a directory that has the old name")]
#[test_case("/d", 1; "into a directory that has another name")]
fn syncing_a_directory_keeps_what_its_names_replaced(dir: &'static str, syncs: u32) -> Result {
    crash(
        move || {
            write("/d", b"other")?;
            write_synced("/n", b"old")?;
            sync_dir("/")?;
            write("/y", b"new")?;
            rename("/y", "/n")?;
            rename("/n", "/tmp")?;
            remove_file(dir).or_else(|_| remove_file("/d"))?;
            create_dir(dir)?;
            rename("/tmp", format!("{dir}/y"))?;
            (0..syncs).try_for_each(|_| sync_dir(dir))
        },
        move || {
            match syncs {
                0 => assert_eq!(read("/n")?, b"old"),
                _ => assert!(exists(format!("{dir}/y"))? && metadata(dir)?.is_dir()),
            }
            assert_eq!(exists("/n")?, dir == "/n");
            assert!(!exists("/tmp")?);
            Ok(())
        },
    )
}

/// A new directory takes a name that the same sync durably frees. The
/// sync makes the directory durable as well, rather than leave that to a
/// second one.
#[test]
fn syncing_a_directory_once_makes_its_own_name_durable() -> Result {
    crash(
        || {
            create_dir("/a")?;
            create_dir("/b")?;
            sync_dir("/")?;
            rename("/a", "/b")?;
            remove_dir("/b")?;
            create_dir_all("/b/c")?;
            sync_dir("/b/c")?;
            rename("/b/c", "/a")?;
            sync_dir("/b")
        },
        || {
            assert!(exists("/a")? && exists("/b")?);
            Ok(())
        },
    )
}

/// Syncing a new directory makes its own name durable, but a name is only
/// reachable after a crash if every directory above it is durable as well.
#[test_case("/a", true; "parent is durable")]
#[test_case("/a/b", false; "parent was never synced")]
fn synced_new_dir_needs_durable_ancestors(synced: &'static str, survives: bool) -> Result {
    crash(
        move || {
            create_dir_all(synced)?;
            sync_dir(synced)
        },
        move || {
            assert_eq!(exists(synced)?, survives);
            assert_eq!(exists("/a")?, survives);
            Ok(())
        },
    )
}

/// A new directory takes the name of one that was renamed away. Until that
/// rename is durable the name still belongs to the old directory.
#[test]
fn synced_new_dir_does_not_displace_a_durable_name() -> Result {
    crash(
        || {
            create_dir("/old")?;
            write_synced("/old/file", b"data")?;
            sync_dir("/")?;
            sync_dir("/old")?;
            rename("/old", "/moved")?;
            create_dir("/old")?;
            sync_dir("/old")
        },
        || {
            assert_eq!(read("/old/file")?, b"data");
            assert!(!exists("/moved")?);
            Ok(())
        },
    )
}

#[test]
fn hard_link_survives_crash() -> Result {
    crash(
        || {
            write_synced("/a", b"AAAA")?;
            hard_link("/a", "/b")?;
            sync_dir("/")?;
            let file = OpenOptions::new().write(true).open("/b")?;
            file.write_all_at(b"ZZ", 0)?;
            file.sync_all()
        },
        || {
            assert_eq!(read("/a")?, b"ZZAA");
            assert_eq!(read("/b")?, b"ZZAA");
            assert_eq!(metadata("/a")?.nlink(), 2);
            Ok(())
        },
    )
}

/// A removed file holds its space until its last descriptor closes, so the
/// second write only fits if the restart closed the descriptor. That goes
/// for a descriptor the software leaked too, as when a process exits.
#[test_case(false, false; "bounce")]
#[test_case(true, false; "crash")]
#[test_case(false, true; "bounce with a leaked file")]
fn restart_closes_open_files(crash: bool, leak: bool) -> Result {
    let mut builder = Builder::new();
    builder.fs().capacity(100);
    restart(
        builder.build(),
        crash,
        move || {
            let file = File::create("/old")?;
            file.write_all_at(&[0; 80], 0)?;
            file.sync_all()?;
            sync_dir("/")?;
            if leak {
                mem::forget(file);
                return Ok(None);
            }
            Ok(Some(file))
        },
        || {
            remove_file("/old")?;
            sync_dir("/")?;
            write("/new", [0; 80])
        },
    )
}

#[test]
fn bounce_preserves_files_opened_by_the_replacement_factory() -> Result {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    let saved = Rc::new(RefCell::new(None::<Fs>));
    let verified = Rc::new(Cell::new(false));
    let mut sim = Builder::new().build();
    sim.host("server", || {
        let opened = saved.borrow().as_ref().map(|fs| {
            let _guard = fs.enter();
            File::create("/new")
        });
        let (saved, verified) = (saved.clone(), verified.clone());
        async move {
            if let Some(file) = opened.transpose()? {
                file.write_all_at(b"new process", 0)?;
                verified.set(true);
            }
            saved.replace(Some(Fs::current()));
            Ok(())
        }
    });
    sim.step()?;
    sim.bounce("server");
    sim.step()?;
    assert!(verified.get());
    Ok(())
}

/// A bounce stops the software as a kill would. What it had written stays,
/// synced or not, but what it had only buffered in memory is lost.
#[test]
fn bounce_does_not_flush_buffered_writes() -> Result {
    restart(
        Builder::new().build(),
        false,
        || {
            write("/written", b"data")?;
            let mut log = BufWriter::new(File::create("/log")?);
            log.write_all(b"buffered")?;
            Ok(log)
        },
        || {
            assert_eq!(read("/written")?, b"data");
            assert_eq!(read("/log")?, b"");
            Ok(())
        },
    )
}

/// Syncing an open directory makes its entries durable. This is how a
/// directory is synced on Linux, where `std::fs` has nothing else for it.
#[test_case(File::sync_all; "sync_all")]
#[test_case(File::sync_data; "sync_data")]
fn syncing_an_open_directory_keeps_its_entries(sync: fn(&File) -> io::Result<()>) -> Result {
    crash(
        move || {
            create_dir("/data")?;
            sync(&File::open("/")?)?;
            write_synced("/data/file", b"kept")?;
            sync(&File::open("/data")?)
        },
        || {
            assert_eq!(read("/data/file")?, b"kept");
            Ok(())
        },
    )
}

/// A removed directory can still be synced through a handle on it, which
/// makes the names that left it durable where they went.
#[test]
fn syncing_a_removed_directory_through_its_handle() -> Result {
    crash(
        || {
            create_dir("/old")?;
            create_dir("/new")?;
            write_synced("/old/file", b"data")?;
            for dir in ["/", "/old"] {
                sync_dir(dir)?;
            }
            let old = File::open("/old")?;
            rename("/old/file", "/new/file")?;
            remove_dir("/old")?;
            old.sync_all()
        },
        || {
            assert_eq!(read("/new/file")?, b"data");
            assert!(!exists("/old/file")?);
            Ok(())
        },
    )
}

/// Removes the directory at `path`. If `held`, a handle is opened on it
/// first and returned, to outlive the name.
fn remove_dir_held(path: &str, held: bool) -> io::Result<Option<File>> {
    let dir = held.then(|| File::open(path)).transpose()?;
    remove_dir(path)?;
    Ok(dir)
}

/// A handle keeps a removed directory's inode, not its place among the
/// names: a new directory that left it becomes durable where it lives.
#[test_case(false; "closed")]
#[test_case(true; "held open")]
fn new_directory_is_not_synced_into_a_removed_parent(held: bool) -> Result {
    restart(
        Builder::new().build(),
        true,
        move || {
            create_dir("/b")?;
            sync_dir("/")?;
            create_dir("/a")?;
            create_dir("/a/c")?;
            rename("/a/c", "/b/c")?;
            let removed = remove_dir_held("/a", held)?;
            sync_dir("/b/c")?;
            Ok(removed)
        },
        || {
            assert!(metadata("/b/c")?.is_dir());
            Ok(())
        },
    )
}

/// A name that was durable only in a directory no crash would find is a
/// new name once that directory is removed, held open or not: syncing it
/// makes nothing above it durable.
#[test_case(false; "closed")]
#[test_case(true; "held open")]
fn sync_reaches_no_further_for_a_removed_directory(held: bool) -> Result {
    restart(
        Builder::new().build(),
        true,
        move || {
            write_synced("/d", b"taken")?;
            sync_dir("/")?;
            remove_file("/d")?;
            create_dir("/d")?;
            write_synced("/d/file", b"data")?;
            sync_dir("/d")?;
            create_dir_all("/g/e")?;
            rename("/d/file", "/g/e/file")?;
            let removed = remove_dir_held("/d", held)?;
            sync_dir("/g/e")?;
            Ok(removed)
        },
        || {
            assert_eq!(read("/d")?, b"taken");
            assert!(!exists("/g")?);
            Ok(())
        },
    )
}

/// Syncing what a handle keeps of a removed directory commits nothing. A
/// new directory that passed through it is still synced under its first
/// name, as when the removed one is gone altogether.
#[test_case(false; "closed")]
#[test_case(true; "held open and synced")]
fn syncing_what_is_left_of_a_removed_directory_commits_nothing(held: bool) -> Result {
    restart(
        Builder::new().build(),
        true,
        move || {
            create_dir("/first")?;
            create_dir("/last")?;
            sync_dir("/")?;
            create_dir("/first/new")?;
            create_dir("/between")?;
            rename("/first/new", "/between/new")?;
            rename("/between/new", "/last/new")?;
            let removed = remove_dir_held("/between", held)?;
            if let Some(removed) = &removed {
                removed.sync_all()?;
            }
            sync_dir("/last/new")?;
            Ok(removed)
        },
        || {
            for path in ["/first/new", "/between", "/last/new"] {
                assert_eq!(exists(path)?, path == "/first/new", "{path}");
            }
            Ok(())
        },
    )
}

#[test]
fn synced_directory_rename_keeps_children() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, read, rename, write};

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
                create_dir("/old")?;
                sync_dir("/")?;
                write("/old/data", b"payload")?;
                let file = OpenOptions::new().write(true).open("/old/data")?;
                file.sync_all()?;
                sync_dir("/old")?;
                rename("/old", "/new")?;
                sync_dir("/")?;
            } else {
                assert!(metadata("/old").is_err());
                assert!(metadata("/old/data").is_err());
                assert_eq!(read("/new/data")?, b"payload");
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

/// Runs `before` on a host, crashes it, then runs `after` on the restarted
/// host so it can inspect what survived.
fn crash_then_check(
    before: fn() -> std::io::Result<()>,
    after: fn() -> std::io::Result<()>,
) -> Result {
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
                before()?;
            } else {
                after()?;
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
fn unsynced_directory_rename_rolls_back() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, read, rename};

    crash_then_check(
        || {
            create_dir("/old")?;
            sync_dir("/")?;
            write_synced("/old/a.txt", b"a")?;
            sync_dir("/old")?;
            rename("/old", "/new")?;
            Ok(())
        },
        || {
            assert!(metadata("/new").is_err());
            assert!(metadata("/new/a.txt").is_err());
            assert_eq!(read("/old/a.txt")?, b"a");
            Ok(())
        },
    )
}

#[test]
fn syncing_renamed_directory_does_not_flush_its_rename() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, read, rename};

    // Syncing the directory under its new name, or one of its
    // subdirectories, only makes the directory's contents durable. The
    // entry for the new name lives in the parent and is still pending.
    for sync_target in ["/new", "/new/sub"] {
        let before = if sync_target == "/new" {
            (|| {
                create_dir_all("/old/sub")?;
                sync_dir("/")?;
                write_synced("/old/sub/a.txt", b"a")?;
                sync_dir("/old")?;
                sync_dir("/old/sub")?;
                rename("/old", "/new")?;
                sync_dir("/new")?;
                Ok(())
            }) as fn() -> std::io::Result<()>
        } else {
            || {
                create_dir_all("/old/sub")?;
                sync_dir("/")?;
                write_synced("/old/sub/a.txt", b"a")?;
                sync_dir("/old")?;
                sync_dir("/old/sub")?;
                rename("/old", "/new")?;
                sync_dir("/new/sub")?;
                Ok(())
            }
        };
        crash_then_check(before, || {
            assert!(metadata("/new").is_err());
            assert_eq!(read("/old/sub/a.txt")?, b"a");
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn directory_rename_does_not_make_unsynced_child_entry_durable() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, rename};

    crash_then_check(
        || {
            create_dir("/old")?;
            sync_dir("/")?;
            // The file's data is synced, but its entry in /old never is.
            write_synced("/old/a.txt", b"a")?;
            rename("/old", "/new")?;
            sync_dir("/")?;
            Ok(())
        },
        || {
            assert!(metadata("/new").is_ok());
            assert!(metadata("/new/a.txt").is_err());
            assert!(metadata("/old").is_err());
            Ok(())
        },
    )
}

#[test]
fn directory_rename_keeps_synced_child_entry_and_drops_unsynced_one() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, read, rename};

    crash_then_check(
        || {
            create_dir("/old")?;
            sync_dir("/")?;
            write_synced("/old/kept.txt", b"kept")?;
            sync_dir("/old")?;
            write_synced("/old/lost.txt", b"lost")?;
            rename("/old", "/new")?;
            sync_dir("/")?;
            Ok(())
        },
        || {
            assert_eq!(read("/new/kept.txt")?, b"kept");
            assert!(metadata("/new/lost.txt").is_err());
            Ok(())
        },
    )
}

#[test]
fn syncing_destination_parent_does_not_flush_unrelated_rename() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, rename};

    crash_then_check(
        || {
            create_dir_all("/a/s")?;
            create_dir("/b")?;
            sync_dir("/")?;
            sync_dir("/a")?;
            rename("/a/s", "/b/s")?;
            // A different directory now sits at the old name, and a file in
            // it is moved into the renamed one. Neither is synced.
            create_dir("/a/s")?;
            write_synced("/a/s/y", b"y")?;
            rename("/a/s/y", "/b/s/y")?;
            sync_dir("/b")?;
            Ok(())
        },
        || {
            assert!(metadata("/b/s").is_ok());
            assert!(metadata("/b/s/y").is_err());
            Ok(())
        },
    )
}

#[test]
fn write_after_synced_directory_rename_survives_crash() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, read, rename};

    crash_then_check(
        || {
            create_dir("/old")?;
            sync_dir("/")?;
            sync_dir("/old")?;
            // Create, write and sync everything below /old, then rename.
            write_synced("/old/a.txt", b"a")?;
            sync_dir("/old")?;
            rename("/old", "/new")?;
            sync_dir("/")?;
            // Changes made under the new name still land on the same file.
            let file = OpenOptions::new().write(true).open("/new/a.txt")?;
            file.write_all_at(b"b", 0)?;
            file.sync_all()?;
            Ok(())
        },
        || {
            assert!(metadata("/old").is_err());
            assert_eq!(read("/new/a.txt")?, b"b");
            Ok(())
        },
    )
}
