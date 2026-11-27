//! Page cache tests.

use std::time::Duration;
use turmoil::fs::shim::tokio::fs as tokio_fs;
use turmoil::{Builder, Result};

#[test]
fn foreign_descriptor_leaves_local_pages_cold() -> Result {
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::FileExt;
    use turmoil::fs::shim::std::fs::OpenOptions;
    use turmoil::fs::Fs;

    let mut builder = Builder::new();
    let latency = Duration::from_millis(10);
    let timing = builder.fs().io_latency();
    timing.min_latency(latency).max_latency(latency);
    builder.fs().page_cache();
    let mut sim = builder.build();
    sim.client("local", async move {
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        let other = Fs::default();
        let foreign = {
            let _guard = other.enter();
            tokio_fs::File::from_std(options.open("/foreign")?)
        };
        let local = options.open("/local")?;
        assert_eq!(local.as_raw_fd(), foreign.as_raw_fd());
        local.write_all_at(b"local", 0)?;
        let local = tokio_fs::File::from_std(local);
        let mut buf = [0; 5];
        let rejected = foreign.read_at(&mut buf, 0).await.unwrap_err();
        assert_eq!(rejected.kind(), ErrorKind::NotFound);
        let rejected = foreign.write_at(b"wrong", 0).await.unwrap_err();
        assert_eq!(rejected.kind(), ErrorKind::NotFound);

        let start = tokio::time::Instant::now();
        local.read_at(&mut buf, 0).await?;
        assert!(start.elapsed() >= latency);
        assert_eq!(&buf, b"local");
        Ok(())
    });
    sim.run()
}

/// Test that writes populate the cache and subsequent reads are fast.
#[test]
fn cache_hit_reduces_latency() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache(); // Enable with defaults

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        // Write populates cache
        let file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;
        file.write_at(b"hello world", 0).await?;

        // Read should hit cache - much faster than 10ms
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 11];
        file.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        // Cache hit should be ~100ns, not 10ms
        assert!(
            elapsed < Duration::from_millis(5),
            "cache hit should be fast, got {:?}",
            elapsed
        );
        assert_eq!(&buf, b"hello world");

        Ok(())
    });
    sim.run()
}

/// Test that without page cache, reads always incur full latency.
#[test]
fn no_cache_full_latency() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    // No page_cache() call - cache disabled

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        let file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;
        file.write_at(b"hello", 0).await?;

        // Without cache, reads always have full latency
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "without cache, read should have full latency, got {:?}",
            elapsed
        );

        Ok(())
    });
    sim.run()
}

/// Test that random eviction causes cache misses.
#[test]
fn random_eviction_chaos() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache().random_eviction_probability(1.0); // Always evict

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        let file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;
        file.write_at(b"hello", 0).await?;

        // With 100% random eviction, reads should always miss cache
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "with 100% eviction, read should have full latency, got {:?}",
            elapsed
        );

        Ok(())
    });
    sim.run()
}

/// Test that O_DIRECT bypasses the page cache.
#[test]
fn direct_io_bypasses_cache() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache();

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        // Write with normal I/O (populates cache) - write a full 512-byte block
        let normal_file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;
        let mut write_data = vec![0u8; 512];
        write_data[..5].copy_from_slice(b"hello");
        normal_file.write_at(&write_data, 0).await?;

        // Open with direct_io - should bypass cache
        // This is the cross-platform way (works on both Linux and macOS)
        let direct_file = tokio_fs::OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open("/data/file.txt")
            .await?;

        // Read with direct_io should have full latency even though data is cached.
        // Use an aligned buffer (512 bytes, aligned to 512).
        let layout = std::alloc::Layout::from_size_align(512, 512).unwrap();
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        let start = tokio::time::Instant::now();
        let buf = unsafe { std::slice::from_raw_parts_mut(ptr, 512) };
        direct_file.read_at(buf, 0).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "direct_io should bypass cache and have full latency, got {:?}",
            elapsed
        );
        assert_eq!(&buf[..5], b"hello");
        unsafe { std::alloc::dealloc(ptr, layout) };

        Ok(())
    });
    sim.run()
}

/// Test LRU eviction when cache is full.
#[test]
fn lru_eviction() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache().max_pages(2); // Very small cache

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        let file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;

        // Write to 3 different pages (cache only holds 2)
        file.write_at(b"page0", 0).await?; // Page 0
        file.write_at(b"page1", 4096).await?; // Page 1
        file.write_at(b"page2", 8192).await?; // Page 2 - evicts page 0

        // Page 0 should have been evicted (LRU)
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "evicted page should have full latency, got {:?}",
            elapsed
        );

        // Page 2 should still be cached
        let start = tokio::time::Instant::now();
        file.read_at(&mut buf, 8192).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_millis(5),
            "cached page should be fast, got {:?}",
            elapsed
        );

        Ok(())
    });
    sim.run()
}

/// Test that cache works across multiple files.
#[test]
fn cache_multiple_files() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache();

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        // Write to two different files
        let file1 = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file1.txt")
            .await?;
        let file2 = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file2.txt")
            .await?;

        file1.write_at(b"file1", 0).await?;
        file2.write_at(b"file2", 0).await?;

        // Both should be cached
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 5];
        file1.read_at(&mut buf, 0).await?;
        file2.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        // Both reads should hit cache
        assert!(
            elapsed < Duration::from_millis(5),
            "both files should be cached, got {:?}",
            elapsed
        );

        Ok(())
    });
    sim.run()
}

/// A removed file gives its pages back, so they cannot push out the pages
/// of files that are still there.
#[test]
fn removed_file_leaves_the_cache() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache().max_pages(2);

    let mut sim = builder.build();
    sim.client("test", async {
        let mut options = tokio_fs::OpenOptions::new();
        options.read(true).write(true).create(true);

        let kept = options.open("/kept").await?;
        kept.write_at(b"kept", 0).await?;
        let removed = options.open("/removed").await?;
        removed.write_at(b"removed", 0).await?;
        drop(removed);
        tokio_fs::remove_file("/removed").await?;

        // Fills the slot the removed file gave up, not the kept file's.
        let new = options.open("/new").await?;
        new.write_at(b"new", 0).await?;

        let start = tokio::time::Instant::now();
        kept.read_at(&mut [0u8; 4], 0).await?;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(5),
            "kept page should still be cached, got {elapsed:?}"
        );
        Ok(())
    });
    sim.run()
}

/// A crash empties the page cache along with the rest of memory.
#[test]
fn crash_empties_the_cache() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));
    builder.fs().page_cache();

    let mut sim = builder.build();
    sim.client("test", async {
        let file = tokio_fs::File::create("/file").await?;
        file.write_at(b"data", 0).await?;
        file.sync_all().await?;
        tokio_fs::sync_dir("/").await?;
        drop(file);

        turmoil::fs::Fs::current().crash();

        let file = tokio_fs::File::open("/file").await?;
        let start = tokio::time::Instant::now();
        file.read_at(&mut [0u8; 4], 0).await?;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(10),
            "read after a crash should miss the cache, got {elapsed:?}"
        );
        Ok(())
    });
    sim.run()
}

/// A write through a descriptor that appends lands at the end of the
/// file, wherever it is aimed, and that is the page it leaves in the cache.
#[test]
fn appended_write_caches_the_page_it_lands_on() -> Result {
    use turmoil::fs::shim::std::fs::{write, OpenOptions};

    let mut builder = Builder::new();
    let latency = Duration::from_millis(10);
    let timing = builder.fs().io_latency();
    timing.min_latency(latency).max_latency(latency);
    builder.fs().page_cache();
    let mut sim = builder.build();
    sim.client("test", async move {
        const END: u64 = 3 * 4096;
        write("/log", vec![0; END as usize])?;
        let mut options = OpenOptions::new();
        let log = options.read(true).append(true).open("/log")?;
        let log = tokio_fs::File::from_std(log);
        log.write_at(b"tail", 0).await?;

        let mut buf = [0; 4];
        let start = tokio::time::Instant::now();
        log.read_at(&mut buf, END).await?;
        let (hit, start) = (start.elapsed(), tokio::time::Instant::now());
        log.read_at(&mut buf, 0).await?;
        assert!(hit < latency && start.elapsed() >= latency, "{hit:?}");
        Ok(())
    });
    sim.run()
}
