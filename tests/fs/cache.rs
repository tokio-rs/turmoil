//! Page cache tests.

use std::time::Duration;
use turmoil::fs::shim::tokio::fs as tokio_fs;
use turmoil::{Builder, Result};

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

        // Write with normal I/O (populates cache)
        let normal_file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")
            .await?;
        normal_file.write_at(b"hello", 0).await?;

        // Open with direct_io - should bypass cache
        // This is the cross-platform way (works on both Linux and macOS)
        let direct_file = tokio_fs::OpenOptions::new()
            .read(true)
            .direct_io(true)
            .open("/data/file.txt")
            .await?;

        // Read with direct_io should have full latency even though data is cached
        let start = tokio::time::Instant::now();
        let mut buf = [0u8; 5];
        direct_file.read_at(&mut buf, 0).await?;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_millis(10),
            "direct_io should bypass cache and have full latency, got {:?}",
            elapsed
        );
        assert_eq!(&buf, b"hello");

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
