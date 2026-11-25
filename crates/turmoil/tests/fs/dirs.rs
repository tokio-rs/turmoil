//! Directory operation tests.

use std::os::unix::fs::FileExt;
use turmoil::fs::shim::std::fs::{
    create_dir, create_dir_all, metadata, read_dir, remove_dir, remove_dir_all, remove_file,
    rename, sync_dir, write, File, OpenOptions,
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
fn rename_dir() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        sync_dir("/")?;
        rename("/old", "/new")?;
        assert!(metadata("/old").is_err());
        let meta = metadata("/new")?;
        assert!(meta.is_dir());
        assert!(!meta.is_file());
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_unsynced_dir() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        rename("/old", "/new")?;
        assert!(metadata("/old").is_err());
        let meta = metadata("/new")?;
        assert!(meta.is_dir());
        assert!(!meta.is_file());
        Ok(())
    });
    sim.run()
}

#[test]
fn repeated_renames_preserve_each_entry_kind() -> Result {
    use turmoil::fs::shim::std::fs::{read_link, symlink, symlink_metadata};

    for synced in [false, true] {
        for kind in 0..3 {
            let mut sim = Builder::new().build();
            sim.client("test", async move {
                match kind {
                    0 => write("/a", b"data")?,
                    1 => create_dir("/a")?,
                    _ => symlink("/target", "/a")?,
                }
                if synced {
                    sync_dir("/")?;
                }
                for n in 0..128 {
                    let (from, to) = if n % 2 == 0 {
                        ("/a", "/b")
                    } else {
                        ("/b", "/a")
                    };
                    rename(from, to)?;
                    assert!(symlink_metadata(from).is_err());
                    let meta = symlink_metadata(to)?;
                    assert_eq!(meta.is_file(), kind == 0);
                    assert_eq!(meta.is_dir(), kind == 1);
                    assert_eq!(meta.is_symlink(), kind == 2);
                    if kind == 2 {
                        assert_eq!(read_link(to)?, std::path::Path::new("/target"));
                    }
                }
                Ok(())
            });
            sim.run()?;
        }
    }
    Ok(())
}

#[test]
fn reused_source_does_not_change_renamed_entry_kind() -> Result {
    use turmoil::fs::shim::std::fs::{symlink, symlink_metadata};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/a")?;
        rename("/a", "/b")?;
        write("/a", b"new file")?;
        assert!(metadata("/a")?.is_file());
        assert!(metadata("/b")?.is_dir());
        rename("/b", "/c")?;
        symlink("/target", "/b")?;
        assert!(metadata("/c")?.is_dir());
        assert!(symlink_metadata("/b")?.is_symlink());
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

#[test]
fn rename_dir_moves_children() -> Result {
    use turmoil::fs::shim::std::fs::read;

    for synced in [false, true] {
        let mut sim = Builder::new().build();
        sim.client("test", async move {
            create_dir_all("/old/sub")?;
            write("/old/a.txt", b"a")?;
            write("/old/sub/b.txt", b"b")?;
            if synced {
                sync_dir("/")?;
                sync_dir("/old")?;
                sync_dir("/old/sub")?;
            }
            rename("/old", "/new")?;

            assert!(metadata("/old").is_err());
            assert!(metadata("/old/a.txt").is_err());
            assert!(metadata("/old/sub").is_err());
            assert!(metadata("/old/sub/b.txt").is_err());

            assert_eq!(read("/new/a.txt")?, b"a");
            assert_eq!(read("/new/sub/b.txt")?, b"b");
            let mut names: Vec<_> = read_dir("/new")?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<std::io::Result<_>>()?;
            names.sort();
            assert_eq!(names, ["a.txt", "sub"]);

            // The old name is free again and starts out empty.
            create_dir("/old")?;
            assert_eq!(read_dir("/old")?.count(), 0);
            Ok(())
        });
        sim.run()?;
    }
    Ok(())
}

#[test]
fn rename_dir_into_itself_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/a/b")?;
        assert!(rename("/a", "/a/b/c").is_err());
        assert!(metadata("/a/b").is_ok());
        Ok(())
    });
    sim.run()
}

#[test]
fn renamed_directory_is_not_empty() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"a")?;
        rename("/old", "/new")?;

        assert!(remove_dir("/new").is_err());
        assert!(metadata("/new/a.txt").is_ok());

        // Another non-empty directory cannot be renamed on top of it.
        create_dir("/other")?;
        write("/other/b.txt", b"b")?;
        assert!(rename("/other", "/new").is_err());
        assert!(metadata("/new/a.txt").is_ok());
        assert!(metadata("/new/b.txt").is_err());
        assert!(metadata("/other/b.txt").is_ok());
        Ok(())
    });
    sim.run()
}

#[test]
fn syncing_parent_after_directory_rename_keeps_children() -> Result {
    use turmoil::fs::shim::std::fs::read;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"a")?;
        rename("/old", "/new")?;
        sync_dir("/")?;

        assert!(metadata("/old/a.txt").is_err());
        assert_eq!(read("/new/a.txt")?, b"a");
        assert_eq!(read_dir("/new")?.count(), 1);
        Ok(())
    });
    sim.run()
}

#[test]
fn open_handle_follows_directory_rename() -> Result {
    use turmoil::fs::shim::std::fs::read;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/old/a.txt")?;
        file.write_all_at(b"one", 0)?;
        rename("/old", "/new")?;

        file.write_all_at(b"two", 0)?;
        file.sync_all()?;
        assert_eq!(read("/new/a.txt")?, b"two");
        assert!(metadata("/old/a.txt").is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn write_after_renaming_synced_directory() -> Result {
    use turmoil::fs::shim::std::fs::read;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"old")?;
        sync_dir("/")?;
        sync_dir("/old")?;
        let file = OpenOptions::new().write(true).open("/old/a.txt")?;
        file.sync_all()?;
        rename("/old", "/new")?;

        write("/new/a.txt", b"new")?;
        assert_eq!(read("/new/a.txt")?, b"new");
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_dir_keeps_child_permissions() -> Result {
    use turmoil::fs::shim::std::fs::{set_permissions, Permissions};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"a")?;
        set_permissions("/old/a.txt", Permissions::from_mode(0o600))?;
        rename("/old", "/new")?;
        sync_dir("/")?;
        assert_eq!(metadata("/new/a.txt")?.permissions().mode() & 0o777, 0o600);
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_dir_keeps_child_hard_links() -> Result {
    use turmoil::fs::shim::std::fs::{hard_link, read};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"a")?;
        hard_link("/old/a.txt", "/old/b.txt")?;
        rename("/old", "/new")?;
        sync_dir("/")?;

        assert!(metadata("/old/b.txt").is_err());
        assert_eq!(read("/new/a.txt")?, b"a");
        assert_eq!(read("/new/b.txt")?, b"a");
        assert_eq!(read_dir("/new")?.count(), 2);
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_dir_twice_then_sync_parent() -> Result {
    use turmoil::fs::shim::std::fs::read;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/old")?;
        write("/old/a.txt", b"a")?;
        rename("/old", "/mid")?;
        write("/mid/b.txt", b"b")?;
        rename("/mid", "/new")?;
        sync_dir("/")?;

        assert!(metadata("/old").is_err());
        assert!(metadata("/mid").is_err());
        assert_eq!(read("/new/a.txt")?, b"a");
        assert_eq!(read("/new/b.txt")?, b"b");
        Ok(())
    });
    sim.run()
}

#[test]
fn open_directory_for_writing_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;

        let err = File::create("/dir").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::IsADirectory);
        assert!(OpenOptions::new().write(true).open("/dir").is_err());
        assert!(OpenOptions::new().append(true).open("/dir").is_err());
        assert!(OpenOptions::new()
            .read(true)
            .create(true)
            .open("/dir")
            .is_err());
        let err = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open("/dir")
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

        // The failed opens must not leave a file behind the directory.
        assert!(metadata("/dir")?.is_dir());
        assert!(remove_file("/dir").is_err());
        remove_dir("/dir")?;
        Ok(())
    });
    sim.run()
}

fn create_dir_all_fails_on_existing_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"data")?;

        let err = create_dir_all("/file").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        let err = create_dir_all("/file/sub").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);

        // The file is untouched and still not a directory.
        assert!(metadata("/file")?.is_file());
        assert!(read_dir("/file").is_err());

        // Existing directories keep succeeding.
        create_dir_all("/dir/a")?;
        create_dir_all("/dir/a")?;
        Ok(())
    });
    sim.run()
}
