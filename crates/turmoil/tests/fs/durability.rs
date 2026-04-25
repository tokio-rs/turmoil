//! Crash consistency and durability tests.

use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;
use turmoil::fs::shim::std::fs::{create_dir, create_dir_all, sync_dir, OpenOptions};
use turmoil::{Builder, Result};

const TEST_PATH: &str = "/test/data.db";

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
