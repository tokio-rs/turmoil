//! Async tokio::fs shim tests.

use std::time::Duration;
use turmoil::fs::shim::tokio::fs as tokio_fs;
use turmoil::{Builder, Result};

#[test]
fn tokio_basic_operations() -> Result {
    use tokio::io::AsyncWriteExt;
    let mut sim = Builder::new().build();

    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;
        tokio_fs::write("/data/file.txt", b"hello world").await?;
        let contents = tokio_fs::read("/data/file.txt").await?;
        assert_eq!(contents, b"hello world");

        let mut file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/rw.txt")
            .await?;
        file.write_all(b"async test").await?;
        file.sync_all().await?;
        Ok(())
    });
    sim.run()
}

#[test]
fn tokio_file_operations() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;
        let file = tokio_fs::File::create("/data/test.txt").await?;
        file.write_at(b"content", 0).await?;
        file.sync_all().await?;
        file.set_len(4).await?;
        assert_eq!(file.metadata().await?.len(), 4);
        Ok(())
    });
    sim.run()
}

#[test]
fn tokio_dir_operations() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::create_dir_all("/a/b/c").await?;
        assert!(tokio_fs::try_exists("/a/b/c").await?);
        tokio_fs::remove_dir_all("/a").await?;
        assert!(!tokio_fs::try_exists("/a").await?);
        Ok(())
    });
    sim.run()
}

#[test]
fn tokio_copy_rename_remove() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::write("/src.txt", b"data").await?;
        tokio_fs::copy("/src.txt", "/copy.txt").await?;
        assert_eq!(tokio_fs::read("/copy.txt").await?, b"data");
        tokio_fs::rename("/copy.txt", "/renamed.txt").await?;
        assert!(!tokio_fs::try_exists("/copy.txt").await?);
        tokio_fs::remove_file("/renamed.txt").await?;
        assert!(!tokio_fs::try_exists("/renamed.txt").await?);
        Ok(())
    });
    sim.run()
}

/// Test that I/O operations incur simulated latency when configured.
#[test]
fn tokio_io_latency() -> Result {
    let mut builder = Builder::new();
    // Configure fixed latency for predictable testing
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(10))
        .max_latency(Duration::from_millis(10));

    let mut sim = builder.build();
    sim.client("test", async {
        let start = tokio::time::Instant::now();

        // Create dir doesn't have latency (not an I/O op)
        tokio_fs::create_dir("/data").await?;

        // Write has latency
        tokio_fs::write("/data/file.txt", b"hello").await?;
        let after_write = start.elapsed();
        assert!(
            after_write >= Duration::from_millis(10),
            "write should have 10ms latency, got {:?}",
            after_write
        );

        // Read has latency
        let _ = tokio_fs::read("/data/file.txt").await?;
        let after_read = start.elapsed();
        assert!(
            after_read >= Duration::from_millis(20),
            "read should add another 10ms, total {:?}",
            after_read
        );

        Ok(())
    });
    sim.run()
}

/// Test that File operations also incur latency.
#[test]
fn tokio_file_io_latency() -> Result {
    let mut builder = Builder::new();
    builder
        .fs()
        .io_latency()
        .min_latency(Duration::from_millis(5))
        .max_latency(Duration::from_millis(5));

    let mut sim = builder.build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;

        // Open with read+write access
        let file = tokio_fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/test.txt")
            .await?;

        let start = tokio::time::Instant::now();

        // write_at has latency
        file.write_at(b"hello", 0).await?;
        assert!(start.elapsed() >= Duration::from_millis(5));

        // sync_all has latency
        file.sync_all().await?;
        assert!(start.elapsed() >= Duration::from_millis(10));

        // read_at has latency
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0).await?;
        assert!(start.elapsed() >= Duration::from_millis(15));

        // metadata has latency
        let _ = file.metadata().await?;
        assert!(start.elapsed() >= Duration::from_millis(20));

        Ok(())
    });
    sim.run()
}

/// Test that I/O operations complete instantly without latency config.
#[test]
fn tokio_no_latency_by_default() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let start = tokio::time::Instant::now();

        tokio_fs::create_dir("/data").await?;
        tokio_fs::write("/data/file.txt", b"hello").await?;
        let _ = tokio_fs::read("/data/file.txt").await?;

        // Without latency config, operations complete within a single tick (1ms default)
        assert!(
            start.elapsed() < Duration::from_millis(5),
            "operations should be nearly instant without latency config"
        );

        Ok(())
    });
    sim.run()
}
