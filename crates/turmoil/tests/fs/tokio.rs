//! Async tokio::fs shim tests.

use std::io::SeekFrom;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, UNIX_EPOCH};
use turmoil::fs::shim::std::fs::{metadata, read_to_string, OpenOptions, Permissions};
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

/// The async handle opens a directory as the sync one does.
#[test]
fn tokio_open_directory() -> Result {
    use std::io::ErrorKind;
    use tokio::io::AsyncReadExt;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::create_dir("/data").await?;
        let mut dir = tokio_fs::File::open("/data").await?;
        assert!(dir.metadata().await?.is_dir());
        dir.sync_all().await?;
        dir.sync_data().await?;

        let mut buf = [0; 8];
        let reads = [dir.read_at(&mut buf, 0).await, dir.read(&mut buf).await];
        for read in reads {
            assert_eq!(read.unwrap_err().kind(), ErrorKind::IsADirectory);
        }
        let read = tokio_fs::read("/data").await.unwrap_err();
        assert_eq!(read.kind(), ErrorKind::IsADirectory);
        Ok(())
    });
    sim.run()
}

/// `try_exists` tells a path that is not there from one that cannot be
/// looked up, as tokio's does.
#[test]
fn tokio_try_exists_reports_what_is_in_the_way() -> Result {
    use std::io::ErrorKind;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::write("/file", b"x").await?;
        assert!(tokio_fs::try_exists("/file").await?);
        assert!(!tokio_fs::try_exists("/missing").await?);
        assert!(!tokio_fs::try_exists("/missing/below").await?);
        let blocked = tokio_fs::try_exists("/file/below").await.unwrap_err();
        assert_eq!(blocked.kind(), ErrorKind::NotADirectory);
        Ok(())
    });
    sim.run()
}

/// `read_dir` hands out its entries as tokio's does: one at a time, each
/// with the path, the name and the inode of what it names.
#[test]
fn tokio_read_dir_hands_out_entries() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        tokio_fs::create_dir_all("/dir/b").await?;
        tokio_fs::write("/dir/a", b"x").await?;
        let mut entries = tokio_fs::read_dir("/dir").await?;
        let mut found = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let (meta, kind) = (entry.metadata().await?, entry.file_type().await?);
            assert_eq!((entry.ino(), kind.is_dir()), (meta.ino(), meta.is_dir()));
            found.push((entry.path(), entry.file_name(), kind.is_dir()));
        }
        found.sort();
        let [a, b] = &found[..] else {
            panic!("two entries, not {found:?}");
        };
        assert_eq!(a, &("/dir/a".into(), "a".into(), false));
        assert_eq!(b, &("/dir/b".into(), "b".into(), true));
        assert!(entries.next_entry().await?.is_none());
        Ok(())
    });
    sim.run()
}

/// The async file does what tokio's does besides I/O: it turns into the
/// sync file and back, sets permissions and times, and takes no more in
/// one read or write than its buffer may hold.
#[test]
fn tokio_file_has_the_methods_of_tokio() -> Result {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mut file = tokio_fs::File::create_new("/file").await?;
        file.write_all(b"0123456789").await?;
        file.set_max_buf_size(4);
        file.seek(SeekFrom::Start(0)).await?;
        let read = file.read(&mut [0; 8]).await?;
        let wrote = file.write(b"abcdefgh").await?;
        assert_eq!((read, wrote, file.max_buf_size()), (4, 4, 4));
        assert_eq!(file.try_clone().await?.max_buf_size(), 4);

        let then = UNIX_EPOCH + Duration::from_secs(100);
        let times = tokio_fs::FileTimes::new().set_accessed(then);
        file.set_times(times.set_modified(then)).await?;
        file.set_permissions(Permissions::from_mode(0o600)).await?;
        let meta = file.metadata().await?;
        assert_eq!((meta.accessed()?, meta.modified()?), (then, then));
        assert_eq!(meta.permissions().mode(), 0o100600);
        file.set_modified(UNIX_EPOCH).await?;
        assert_eq!(file.metadata().await?.modified()?, UNIX_EPOCH);

        let file = tokio_fs::File::from(file.into_std().await);
        let file = file.try_into_std().expect("no operation is in flight");
        assert_eq!(read_to_string("/file")?, "0123abcd89");
        assert_eq!(file.metadata()?.len(), 10);
        Ok(())
    });
    sim.run()
}

/// The async builders take a mode themselves, as tokio's do, and options
/// convert from the sync ones.
#[test]
fn tokio_builders_take_a_mode() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mut options = tokio_fs::File::options();
        options.write(true).create(true).mode(0o600).custom_flags(0);
        options.open("/file").await?;
        let mut builder = tokio_fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create("/dir/sub").await?;
        let modes = [metadata("/file")?, metadata("/dir")?, metadata("/dir/sub")?];
        let modes = modes.map(|meta| meta.permissions().mode() & 0o7777);
        assert_eq!(modes, [0o600, 0o700, 0o700]);

        let mut options = OpenOptions::new();
        let options = tokio_fs::OpenOptions::from(options.read(true).clone());
        assert!(options.open("/file").await?.metadata().await?.is_file());
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
