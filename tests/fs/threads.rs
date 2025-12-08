//! Worker thread filesystem access tests.
//!
//! These tests demonstrate using `FsHandle` to perform filesystem
//! operations from worker threads spawned outside the turmoil
//! simulation context.

use std::os::unix::fs::FileExt;
use turmoil::fs::shim::std::fs::{create_dir, read, write, OpenOptions};
use turmoil::fs::FsHandle;
use turmoil::{Builder, Result};

#[test]
fn worker_thread_basic_io() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;

        // Capture handle to current host's filesystem
        let handle = FsHandle::current();

        // Spawn worker thread
        let worker = std::thread::spawn(move || {
            // Enter the filesystem context
            let _guard = handle.enter();

            // Now filesystem operations work
            write("/data/from_worker.txt", b"hello from worker")?;
            Ok::<_, std::io::Error>(())
        });

        worker.join().unwrap()?;

        // Data is visible from main thread
        assert_eq!(read("/data/from_worker.txt")?, b"hello from worker");
        Ok(())
    });
    sim.run()
}

#[test]
fn worker_thread_file_handle() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;

        let handle = FsHandle::current();

        let worker = std::thread::spawn(move || {
            let _guard = handle.enter();

            // Use OpenOptions and FileExt trait
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/data/file.txt")?;

            file.write_all_at(b"positioned write", 0)?;
            file.sync_all()?;

            let mut buf = [0u8; 16];
            file.read_at(&mut buf, 0)?;
            assert_eq!(&buf, b"positioned write");

            Ok::<_, std::io::Error>(())
        });

        worker.join().unwrap()?;

        // Verify from main thread
        assert_eq!(read("/data/file.txt")?, b"positioned write");
        Ok(())
    });
    sim.run()
}

#[test]
fn worker_thread_multiple_operations() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        // Create initial data from main thread
        create_dir("/data")?;
        write("/data/initial.txt", b"initial data")?;

        let handle = FsHandle::current();

        let worker = std::thread::spawn(move || {
            let _guard = handle.enter();

            // Read existing data
            let initial = read("/data/initial.txt")?;
            assert_eq!(initial, b"initial data");

            // Create multiple files
            write("/data/file1.txt", b"file one")?;
            write("/data/file2.txt", b"file two")?;
            write("/data/file3.txt", b"file three")?;

            Ok::<_, std::io::Error>(())
        });

        worker.join().unwrap()?;

        // Verify all files from main thread
        assert_eq!(read("/data/file1.txt")?, b"file one");
        assert_eq!(read("/data/file2.txt")?, b"file two");
        assert_eq!(read("/data/file3.txt")?, b"file three");
        Ok(())
    });
    sim.run()
}

#[test]
fn worker_thread_guard_scoping() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;

        let handle = FsHandle::current();

        let worker = std::thread::spawn(move || {
            // First scope
            {
                let _guard = handle.enter();
                write("/data/scoped1.txt", b"scope 1")?;
            }

            // Re-enter with new guard
            {
                let _guard = handle.enter();
                write("/data/scoped2.txt", b"scope 2")?;
            }

            Ok::<_, std::io::Error>(())
        });

        worker.join().unwrap()?;

        assert_eq!(read("/data/scoped1.txt")?, b"scope 1");
        assert_eq!(read("/data/scoped2.txt")?, b"scope 2");
        Ok(())
    });
    sim.run()
}

#[test]
fn worker_thread_with_sync() -> Result {
    use turmoil::fs::shim::std::fs::sync_dir;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        sync_dir("/")?;

        let handle = FsHandle::current();

        let worker = std::thread::spawn(move || {
            let _guard = handle.enter();

            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/data/durable.txt")?;

            file.write_all_at(b"durable data", 0)?;
            file.sync_all()?;
            sync_dir("/data")?;

            Ok::<_, std::io::Error>(())
        });

        worker.join().unwrap()?;

        assert_eq!(read("/data/durable.txt")?, b"durable data");
        Ok(())
    });
    sim.run()
}
