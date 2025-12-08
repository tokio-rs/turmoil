//! Metadata and timestamp tests.

use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::time::{Duration, UNIX_EPOCH};
use tokio::time::sleep;
use turmoil::fs::shim::std::fs::{
    create_dir, create_dir_all, exists, metadata, remove_file, symlink, symlink_metadata, write,
    OpenOptions,
};
use turmoil::{Builder, Result};

#[test]
fn metadata_is_file_is_dir() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        let meta = file.metadata()?;
        assert!(meta.is_file());
        assert!(!meta.is_dir());
        Ok(())
    });
    sim.run()
}

#[test]
fn metadata_free_function() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/data")?;
        write("/data/file.txt", b"hello world")?;
        let file_meta = metadata("/data/file.txt")?;
        assert!(file_meta.is_file());
        assert_eq!(file_meta.len(), 11);
        let dir_meta = metadata("/data")?;
        assert!(dir_meta.is_dir());
        let root_meta = metadata("/")?;
        assert!(root_meta.is_dir());
        assert!(metadata("/nonexistent").is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn timestamp_on_creation() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        let meta = file.metadata()?;
        let created = meta.created()?;
        let modified = meta.modified()?;
        assert!(created > UNIX_EPOCH);
        assert_eq!(created, modified);
        Ok(())
    });
    sim.run()
}

#[test]
fn mtime_updates_on_write() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        let initial = file.metadata()?.modified()?;
        sleep(Duration::from_secs(1)).await;
        file.write_all_at(b"hello", 0)?;
        let after = file.metadata()?.modified()?;
        assert!(after > initial);
        Ok(())
    });
    sim.run()
}

#[test]
fn mtime_updates_on_truncate() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file.write_all_at(b"hello world", 0)?;
        file.sync_all()?;
        let before = file.metadata()?.modified()?;
        sleep(Duration::from_secs(1)).await;
        file.set_len(5)?;
        let after = file.metadata()?.modified()?;
        assert!(after > before);
        Ok(())
    });
    sim.run()
}

#[test]
fn noatime_behavior() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file.write_all_at(b"hello", 0)?;
        let before = file.metadata()?.accessed()?;
        sleep(Duration::from_secs(1)).await;
        let mut buf = [0u8; 5];
        file.read_at(&mut buf, 0)?;
        let after = file.metadata()?.accessed()?;
        assert_eq!(before, after, "noatime: accessed should not change on read");
        Ok(())
    });
    sim.run()
}

#[test]
fn directory_timestamps() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/mydir")?;
        let meta = metadata("/mydir")?;
        assert!(meta.is_dir());
        assert!(meta.created()? > UNIX_EPOCH);
        sleep(Duration::from_secs(1)).await;
        create_dir("/mydir/subdir")?;
        let sub_meta = metadata("/mydir/subdir")?;
        assert!(sub_meta.created()? > meta.created()?);
        Ok(())
    });
    sim.run()
}

#[test]
fn dir_mtime_updates_on_content_change() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let initial = metadata("/data")?.modified()?;
        sleep(Duration::from_secs(1)).await;
        write("/data/file.txt", b"hello")?;
        let after_create = metadata("/data")?.modified()?;
        assert!(after_create > initial);
        sleep(Duration::from_secs(1)).await;
        remove_file("/data/file.txt")?;
        let after_remove = metadata("/data")?.modified()?;
        assert!(after_remove >= after_create);
        Ok(())
    });
    sim.run()
}

#[test]
fn permissions_from_metadata() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;

        // Test default file permissions (0o644)
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/default.txt")?;
        let perms = file.metadata()?.permissions();
        assert_eq!(perms.mode() & 0o777, 0o644);

        // Test custom file permissions via OpenOptionsExt
        let file2 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open("/data/custom.txt")?;
        let perms2 = file2.metadata()?.permissions();
        assert_eq!(perms2.mode() & 0o777, 0o600);

        // Test directory permissions (0o755 default)
        let dir_meta = metadata("/data")?;
        assert_eq!(dir_meta.permissions().mode() & 0o777, 0o755);

        Ok(())
    });
    sim.run()
}

#[test]
fn exists_function() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        // Non-existent paths
        assert!(!exists("/nonexistent"));
        assert!(!exists("/data/file.txt"));

        // Create directory
        create_dir("/data")?;
        assert!(exists("/data"));

        // Create file
        write("/data/file.txt", b"hello")?;
        assert!(exists("/data/file.txt"));

        // Create symlink to existing target
        symlink("/data/file.txt", "/link")?;
        assert!(exists("/link"));

        // Symlink to non-existent target should return false
        symlink("/nonexistent", "/broken_link")?;
        assert!(!exists("/broken_link"));

        Ok(())
    });
    sim.run()
}

#[test]
fn symlink_metadata_timestamps() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/target.txt", b"hello")?;
        sleep(Duration::from_secs(1)).await;
        symlink("/target.txt", "/link")?;

        let link_meta = symlink_metadata("/link")?;
        assert!(link_meta.is_symlink());
        assert!(!link_meta.is_file());
        assert!(!link_meta.is_dir());

        // Symlink should have its own timestamp
        let link_created = link_meta.created()?;
        assert!(link_created > UNIX_EPOCH);

        // Target file should have earlier timestamp
        let target_meta = metadata("/target.txt")?;
        assert!(target_meta.created()? < link_created);

        Ok(())
    });
    sim.run()
}
