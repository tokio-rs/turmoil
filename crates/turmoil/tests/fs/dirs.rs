//! Directory operation tests.

use std::os::unix::fs::FileExt;
use turmoil::fs::shim::std::fs::{
    create_dir, create_dir_all, metadata, read_dir, remove_dir, remove_dir_all, remove_file,
    rename, write, OpenOptions,
};
use turmoil::{Builder, Result};

#[test]
fn mkdir_rmdir() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/mydir")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/mydir/file.txt")?;
        file.write_all_at(b"hello", 0)?;
        assert!(remove_dir("/mydir").is_err()); // not empty
        drop(file);
        remove_file("/mydir/file.txt")?;
        remove_dir("/mydir")?;
        Ok(())
    });
    sim.run()
}

#[test]
fn mkdir_requires_parent() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        assert!(create_dir("/a/b/c").is_err());
        create_dir("/a")?;
        create_dir("/a/b")?;
        create_dir("/a/b/c")?;
        Ok(())
    });
    sim.run()
}

#[test]
fn create_dir_all_works() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/a/b/c/d")?;
        let _file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/a/b/c/d/file.txt")?;
        Ok(())
    });
    sim.run()
}

#[test]
fn remove_file_basic() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file.txt", b"hello")?;
        remove_file("/data/file.txt")?;
        assert!(OpenOptions::new()
            .read(true)
            .open("/data/file.txt")
            .is_err());
        assert!(remove_file("/data/file.txt").is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn read_dir_basic() -> Result {
    use std::collections::HashSet;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/data/subdir")?;
        write("/data/file1.txt", b"content1")?;
        write("/data/file2.txt", b"content2")?;
        let entries: Vec<_> = read_dir("/data")?.collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(entries.len(), 3);
        let names: HashSet<_> = entries
            .iter()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            names.contains("subdir") && names.contains("file1.txt") && names.contains("file2.txt")
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn remove_dir_all_basic() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/data/level1/level2")?;
        write("/data/file1.txt", b"hello")?;
        write("/data/level1/file2.txt", b"world")?;
        remove_dir_all("/data")?;
        assert!(metadata("/data").is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn remove_dir_all_empty() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/empty")?;
        remove_dir_all("/empty")?;
        assert!(metadata("/empty").is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/old.txt", b"hello")?;
        rename("/data/old.txt", "/data/new.txt")?;
        assert!(OpenOptions::new().read(true).open("/data/old.txt").is_err());
        assert!(metadata("/data/new.txt").is_ok());
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_atomic_replace() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file1.txt", b"original")?;
        write("/data/file2.txt", b"replacement")?;
        rename("/data/file2.txt", "/data/file1.txt")?;
        use turmoil::fs::shim::std::fs::read;
        assert_eq!(read("/data/file1.txt")?, b"replacement");
        assert!(OpenOptions::new()
            .read(true)
            .open("/data/file2.txt")
            .is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn remove_dir_all_nonexistent() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        assert!(remove_dir_all("/nonexistent").is_err());
        Ok(())
    });
    sim.run()
}
