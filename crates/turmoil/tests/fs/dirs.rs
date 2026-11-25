//! Directory operation tests.

use std::ffi::OsString;
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex};
use turmoil::fs::shim::std::fs::{
    copy, create_dir, create_dir_all, exists, hard_link, metadata, read, read_dir, read_to_string,
    remove_dir, remove_dir_all, remove_file, rename, sync_dir, write, File, FileType, OpenOptions,
};
use turmoil::fs::shim::std::os::unix::fs::symlink;
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
    use turmoil::fs::shim::std::fs::{read_link, symlink_metadata};
    use turmoil::fs::shim::std::os::unix::fs::symlink;

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
    use turmoil::fs::shim::std::fs::symlink_metadata;
    use turmoil::fs::shim::std::os::unix::fs::symlink;

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
fn rename_dir_moves_its_contents() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/a/sub")?;
        write("/a/file", b"one")?;
        write("/a/sub/file", b"two")?;
        rename("/a", "/b")?;
        assert_eq!(read("/b/file")?, b"one");
        assert_eq!(read("/b/sub/file")?, b"two");
        assert!(metadata("/a").is_err());
        assert!(metadata("/a/file").is_err());
        let mut names: Vec<_> = read_dir("/b")?.map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["file", "sub"]);
        Ok(())
    });
    sim.run()
}

#[test]
fn rename_dir_into_itself_fails() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/a/b")?;
        let err = rename("/a", "/a/b/c").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(metadata("/a/b")?.is_dir());
        Ok(())
    });
    sim.run()
}

#[test]
fn paths_resolve_through_symlinked_dirs_and_dot_dot() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file", b"x")?;
        symlink("/data", "/link")?;
        assert_eq!(read("/link/file")?, b"x");
        assert_eq!(read("/data/../link/./file")?, b"x");
        create_dir("/link/sub")?;
        assert!(metadata("/data/sub")?.is_dir());
        Ok(())
    });
    sim.run()
}

/// The same simulation must list a directory in the same order every time.
#[test]
fn read_dir_order_is_reproducible() -> Result {
    fn listing() -> Result<Vec<OsString>> {
        let names = Arc::new(Mutex::new(Vec::new()));
        let listed = names.clone();
        let mut sim = Builder::new().build();
        sim.client("test", async move {
            for n in 0..16 {
                write(format!("/file{n}"), b"")?;
            }
            let entries = read_dir("/")?.map(|e| e.unwrap().file_name());
            listed.lock().unwrap().extend(entries);
            Ok(())
        });
        sim.run()?;
        let names = names.lock().unwrap().clone();
        Ok(names)
    }

    let first = listing()?;
    assert_eq!(first.len(), 16);
    assert_eq!(first, listing()?);
    Ok(())
}

/// A path that ends in `.` or `..` leads to a directory but names no entry
/// of it, so nothing is renamed or removed by it. Each call fails the way
/// it does on Linux.
#[test]
fn dot_and_dot_dot_name_no_entry() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/a/b")?;
        write("/a/keep", b"x")?;
        let refused = [
            (rename("/a/b/..", "/c"), ErrorKind::ResourceBusy),
            (rename("/a/b/.", "/c"), ErrorKind::ResourceBusy),
            (rename("/a/keep", "/a/b/.."), ErrorKind::ResourceBusy),
            (remove_file("/a/b/."), ErrorKind::IsADirectory),
            (remove_file("/a/b/.."), ErrorKind::IsADirectory),
            (remove_dir("/a/b/."), ErrorKind::InvalidInput),
            (remove_dir("/a/b/.."), ErrorKind::DirectoryNotEmpty),
            (remove_dir("/a/b/../b/./"), ErrorKind::InvalidInput),
            (remove_dir("/"), ErrorKind::ResourceBusy),
        ];
        for (index, (refused, kind)) in refused.into_iter().enumerate() {
            assert_eq!(refused.unwrap_err().kind(), kind, "{index}");
        }
        assert_eq!(read("/a/b/../keep")?, b"x");
        assert!(metadata("/a/b/.")?.is_dir());
        assert!(!exists("/c")?);
        Ok(())
    });
    sim.run()
}

/// A rename examines its paths in the order Linux does: both are followed
/// first, then both have to end in a name, and only then does the source
/// have to exist and be what a trailing slash asks for.
#[test]
fn rename_examines_its_paths_in_order() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/file", b"x")?;
        symlink("/nowhere", "/dangling")?;
        let refused = [
            (rename("/dangling/", "/dangling/in"), ErrorKind::NotFound),
            (rename("/file/", "/missing/in"), ErrorKind::NotFound),
            (rename("/missing", "/dir/.."), ErrorKind::ResourceBusy),
            (rename("/missing", "/moved"), ErrorKind::NotFound),
            (rename("/file/", "/moved"), ErrorKind::NotADirectory),
            (rename("/file", "/moved/"), ErrorKind::NotADirectory),
            (rename("/dangling/", "/moved"), ErrorKind::NotADirectory),
        ];
        for (index, (refused, kind)) in refused.into_iter().enumerate() {
            assert_eq!(refused.unwrap_err().kind(), kind, "{index}");
        }
        rename("/dir/", "/moved/")?;
        assert_eq!(read_dir("/")?.count(), 3);
        Ok(())
    });
    sim.run()
}

/// `remove_dir_all` goes about it as `std::fs` does on Linux. It empties
/// what the path leads to, then removes the directory by that same path:
/// which can fail after the fact, or find nothing left to remove.
#[test]
fn remove_dir_all_empties_before_it_removes() -> Result {
    use turmoil::fs::shim::std::fs::symlink_metadata;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        for dir in ["/dot", "/up", "/full"] {
            create_dir_all(format!("{dir}/sub"))?;
            write(format!("{dir}/file"), b"x")?;
        }
        symlink("/full", "/link")?;
        symlink("/nowhere", "/dangling")?;
        let emptied = |dir: &str| read_dir(dir).map(|entries| entries.count() == 0);

        let dot = remove_dir_all("/dot/.").unwrap_err();
        assert_eq!(dot.kind(), ErrorKind::InvalidInput);
        assert!(emptied("/dot")?);

        // The way up is gone with what was removed, which is no failure.
        remove_dir_all("/up/sub/..")?;
        assert!(emptied("/up")?);

        // A slash follows the link to empty the directory, but removes neither.
        let followed = remove_dir_all("/link/").unwrap_err();
        assert_eq!(followed.kind(), ErrorKind::NotADirectory);
        assert!(emptied("/full")?);
        let nothing = remove_dir_all("/dangling/").unwrap_err();
        assert_eq!(nothing.kind(), ErrorKind::NotFound);

        // Without the slash it is the link that goes.
        for link in ["/link", "/dangling"] {
            assert!(symlink_metadata(link)?.is_symlink());
            remove_dir_all(link)?;
            assert!(!exists(link)?);
        }
        assert!(metadata("/full")?.is_dir());
        Ok(())
    });
    sim.run()
}

#[test]
fn trailing_slash_and_dot_require_a_directory() -> Result {
    use turmoil::fs::shim::std::fs::symlink_metadata;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"kept")?;
        symlink("/file", "/link")?;
        for path in ["/file/", "/file/.", "/link/", "/link/.//"] {
            assert_eq!(metadata(path).unwrap_err().kind(), ErrorKind::NotADirectory);
            let removal = remove_dir_all(path).unwrap_err();
            assert_eq!(removal.kind(), ErrorKind::NotADirectory);
        }
        assert_eq!(read("/link")?, b"kept");
        symlink("/file/", "/indirect")?;
        assert_eq!(
            metadata("/indirect").unwrap_err().kind(),
            ErrorKind::NotADirectory
        );
        assert!(write("/missing/", []).is_err());
        assert!(!exists("/missing")?);

        create_dir("/dir/")?;
        symlink("/dir", "/dirlink")?;
        assert!(symlink_metadata("/dirlink/")?.is_dir());
        assert!(remove_dir_all("/dir/.").is_err());
        rename("/dir/", "/renamed/")?;
        remove_dir_all("/renamed/")?;
        Ok(())
    });
    sim.run()
}

/// A name is taken by whatever has it, also when the path ends in a slash
/// and what has the name is no directory. Nothing is created over it, and
/// as on Linux no open creates through such a path at all.
#[test]
fn trailing_slash_creates_nothing_over_a_taken_name() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/file", b"kept")?;
        write("/other", b"x")?;
        symlink("/file", "/to_file")?;
        symlink("/dir", "/to_dir")?;
        symlink("/nowhere", "/dangling")?;
        let kind = |refused: std::io::Result<()>| refused.unwrap_err().kind();
        for taken in ["/file/", "/to_file/", "/to_dir/", "/dangling/"] {
            let made = [
                create_dir(taken),
                symlink("/other", taken),
                hard_link("/other", taken),
            ];
            for made in made {
                assert_eq!(kind(made), ErrorKind::AlreadyExists, "{taken}");
            }
            let exclusive = OpenOptions::new().write(true).create_new(true).open(taken);
            let opened = [
                File::create(taken).map(drop),
                write(taken, b"x"),
                exclusive.map(drop),
            ];
            for opened in opened {
                assert_eq!(kind(opened), ErrorKind::IsADirectory, "{taken}");
            }
        }
        for taken in ["/file/", "/to_file/", "/dangling/"] {
            let made = create_dir_all(taken);
            assert_eq!(kind(made), ErrorKind::AlreadyExists, "{taken}");
        }
        create_dir_all("/to_dir/")?;

        assert_eq!(read("/file")?, b"kept");
        assert!(!exists("/nowhere")?);
        assert_eq!(read_dir("/dir")?.count(), 0);
        Ok(())
    });
    sim.run()
}

/// A name holds up to 255 bytes and a path fewer than 4096, as on Linux,
/// and neither holds a NUL.
#[test]
fn names_and_paths_have_the_limits_of_linux() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/short", b"x")?;
        let (longest, too_long) = ("x".repeat(255), format!("/{}", "x".repeat(256)));
        write(format!("/{longest}"), b"x")?;
        create_dir(format!("/dir/{longest}"))?;
        // A path of so many bytes, in names that are as long as they get.
        let deep = |bytes: usize| {
            let mut path = String::from("/deep");
            while path.len() < bytes {
                let name = "d".repeat((bytes - path.len() - 1).min(255));
                path = format!("{path}/{name}");
            }
            path
        };
        create_dir_all(deep(4095))?;
        assert!(metadata(deep(4095))?.is_dir());
        remove_dir_all("/deep")?;
        symlink("t".repeat(4095), "/far")?;

        let refused = [
            write(&too_long, b"x"),
            metadata(&too_long).map(drop),
            create_dir(&too_long),
            create_dir_all(format!("{too_long}/below")),
            symlink("/short", &too_long),
            hard_link("/short", &too_long),
            rename("/short", &too_long),
            rename(&too_long, "/moved"),
            remove_file(&too_long),
            metadata(format!("{too_long}/below")).map(drop),
            create_dir_all(deep(4096)),
            metadata("/dir".to_owned() + &"/.".repeat(2046)).map(drop),
            symlink("t".repeat(4096), "/farther"),
        ];
        for (index, refused) in refused.into_iter().enumerate() {
            let error = refused.unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ENAMETOOLONG), "{index}");
        }
        // What is in the way of a name is found before the name is measured.
        let through = metadata(format!("/short{too_long}")).unwrap_err();
        assert_eq!(through.kind(), ErrorKind::NotADirectory);

        let nul = [
            write("/nu\0l", b"x"),
            metadata("/\0").map(drop),
            symlink("nu\0l", "/link"),
            symlink("/short", "/nu\0l"),
            rename("/short", "/nu\0l"),
        ];
        for (index, nul) in nul.into_iter().enumerate() {
            assert_eq!(nul.unwrap_err().kind(), ErrorKind::InvalidInput, "{index}");
        }
        assert_eq!(read_dir("/")?.count(), 4);
        Ok(())
    });
    sim.run()
}

/// The empty path names nothing, as with `std::fs`. It is what
/// `Path::parent` returns for a bare file name.
#[test]
fn empty_path_is_not_found() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let results = [
            metadata("").map(drop),
            read_dir("").map(drop),
            sync_dir(""),
            remove_dir_all(""),
        ];
        for result in results {
            assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
        }
        assert!(!exists("")?);
        create_dir_all("")?;
        Ok(())
    });
    sim.run()
}

/// As on Linux, a directory opens only to be read. An open that could
/// change or create a file fails, and leaves the directory as it was.
#[test]
fn directory_opens_only_for_reading() -> Result {
    type Options = fn(&mut OpenOptions) -> &mut OpenOptions;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/dir/kept", b"x")?;
        symlink("/dir", "/link")?;
        let refused: [Options; 6] = [
            |open| open.write(true),
            |open| open.append(true),
            |open| open.read(true).write(true),
            |open| open.write(true).create(true),
            |open| open.write(true).truncate(true),
            |open| open.append(true).create(true),
        ];
        // An exclusive create looks no further than a name that is taken,
        // unless the name ends in a slash and so could only be a directory's.
        let taken = [
            ("/dir", ErrorKind::AlreadyExists),
            ("/dir/", ErrorKind::IsADirectory),
            ("/link", ErrorKind::AlreadyExists),
        ];
        for (path, exclusive) in taken {
            for options in refused {
                let refusal = options(&mut OpenOptions::new()).open(path).unwrap_err();
                assert_eq!(refusal.kind(), ErrorKind::IsADirectory, "{path}");
            }
            let created = File::create(path).unwrap_err();
            assert_eq!(created.kind(), ErrorKind::IsADirectory, "{path}");
            let created = OpenOptions::new().write(true).create_new(true).open(path);
            assert_eq!(created.unwrap_err().kind(), exclusive, "{path}");
            assert!(File::open(path)?.metadata()?.is_dir(), "{path}");
        }

        assert!(metadata("/dir")?.is_dir());
        let removal = remove_file("/dir").unwrap_err();
        assert_eq!(removal.kind(), ErrorKind::IsADirectory);
        assert_eq!(read("/dir/kept")?, b"x");
        Ok(())
    });
    sim.run()
}

/// An open directory can be described and synced, but has no bytes.
#[test]
fn open_directory_has_no_bytes() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/file", b"x")?;
        let mut dir = File::open("/dir")?;
        let meta = dir.metadata()?;
        assert!(meta.is_dir());
        assert_eq!((meta.ino(), meta.nlink()), (metadata("/dir")?.ino(), 2));
        assert!(dir.try_clone()?.metadata()?.is_dir());

        let mut buf = [0; 8];
        let reads = [
            dir.read(&mut buf),
            dir.read_at(&mut buf, 0),
            dir.read_at(&mut [], 0),
            read("/dir").map(|bytes| bytes.len()),
            read_to_string("/dir").map(|text| text.len()),
        ];
        for read in reads {
            assert_eq!(read.unwrap_err().kind(), ErrorKind::IsADirectory);
        }
        assert!(dir.write_at(b"x", 0).is_err());
        assert!(dir.set_len(0).is_err());
        dir.sync_all()?;
        dir.sync_data()?;

        // Only a regular file can be copied, and only onto one.
        let copied = copy("/dir", "/copy").unwrap_err();
        assert_eq!(copied.kind(), ErrorKind::InvalidInput);
        assert!(!exists("/copy")?);
        for onto in [copy("/file", "/dir").map(drop), write("/dir", b"x")] {
            assert_eq!(onto.unwrap_err().kind(), ErrorKind::IsADirectory);
        }
        Ok(())
    });
    sim.run()
}

/// Reading a directory fails before it could reach the disk.
#[test]
fn reading_a_directory_is_no_io_error() -> Result {
    let mut builder = Builder::new();
    builder.fs().io_error_probability(1.0);
    let mut sim = builder.build();
    sim.client("test", async {
        create_dir("/dir")?;
        let read = File::open("/dir")?.read_at(&mut [0; 8], 0);
        assert_eq!(read.unwrap_err().kind(), ErrorKind::IsADirectory);
        Ok(())
    });
    sim.run()
}

/// A directory removed while it is open lives on behind its handle, with
/// no name and nothing in it, until the handle is closed.
#[test]
fn removed_directory_stays_open() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        let dir = File::open("/dir")?;
        let ino = dir.metadata()?.ino();
        remove_dir("/dir")?;
        assert!(!exists("/dir")?);

        let meta = dir.metadata()?;
        assert!(meta.is_dir());
        assert_eq!((meta.ino(), meta.nlink()), (ino, 0));
        dir.sync_all()?;

        create_dir("/dir")?;
        write("/dir/file", b"x")?;
        sync_dir("/")?;
        assert_ne!(metadata("/dir")?.ino(), ino);
        dir.sync_all()?;
        assert_eq!(dir.metadata()?.nlink(), 0);
        assert_eq!(read("/dir/file")?, b"x");
        Ok(())
    });
    sim.run()
}

/// `create_dir_all` fails on a file in its way, as `std::fs` does: one at
/// the end of the path already exists, and one before that is no directory.
#[test]
fn create_dir_all_stops_at_a_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/dir/file", b"x")?;
        let failures = [
            ("/dir/file", ErrorKind::AlreadyExists),
            ("/dir/file/sub", ErrorKind::NotADirectory),
            ("/dir/file/sub/deeper", ErrorKind::NotADirectory),
        ];
        for (path, kind) in failures {
            assert_eq!(create_dir_all(path).unwrap_err().kind(), kind, "{path}");
        }
        let existing = create_dir("/dir").unwrap_err();
        assert_eq!(existing.kind(), ErrorKind::AlreadyExists);
        create_dir_all("/dir")?;
        create_dir_all("/dir/sub/deeper")?;

        assert_eq!(read("/dir/file")?, b"x");
        assert!(metadata("/dir/file")?.is_file());
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

/// A directory entry tells the inode it names, and its file type tells of
/// the kinds of file that the simulation has none of.
#[test]
fn dir_entry_tells_its_inode() -> Result {
    use std::os::unix::fs::{DirEntryExt, FileTypeExt};
    use turmoil::fs::shim::std::fs::symlink_metadata;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/dir/file", b"x")?;
        hard_link("/dir/file", "/dir/twin")?;
        symlink("/dir/file", "/dir/link")?;
        let mut inos = Vec::new();
        for entry in read_dir("/dir")? {
            let (entry, kind) = (entry?, symlink_metadata("/dir")?.file_type());
            assert_eq!(entry.ino(), symlink_metadata(entry.path())?.ino());
            let kind = [entry.file_type()?, kind];
            assert!(kind.iter().all(|kind| !kind.is_fifo() && !kind.is_socket()));
            let device = |kind: &FileType| kind.is_block_device() || kind.is_char_device();
            assert!(!kind.iter().any(device));
            inos.push((entry.file_name(), entry.ino()));
        }
        inos.sort();
        let [(_, file), (_, link), (_, twin)] = inos[..] else {
            panic!("three entries, not {inos:?}");
        };
        assert!(file == twin && file != link);
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

#[test]
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

/// What a path ends in is for each call to deal with, once the path is
/// followed that far. Where Linux knows what is wrong with the end of a
/// path before it looks the last name up, that is the error.
#[test]
fn calls_have_their_own_rules_for_the_end_of_a_path() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        symlink("/loop", "/loop")?;
        let long = format!("/{}", "x".repeat(256));
        let kind = |result: io::Result<()>| result.unwrap_err().kind();
        let create_new = |path: &str| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true).open(path).map(drop)
        };
        // A directory that is there is in the way of a new file, whatever
        // the path calls it.
        for path in ["/", "//", "/dir/./", "/dir/../"] {
            assert_eq!(kind(create_new(path)), ErrorKind::AlreadyExists, "{path}");
        }
        // So is a symlink, even one that leads nowhere.
        symlink("/nowhere", "/dangling")?;
        assert_eq!(kind(create_new("/dangling")), ErrorKind::AlreadyExists);
        assert!(!exists("/nowhere")?);
        // No open creates what a slash comes after, and none looks for it.
        for path in ["/loop/", &format!("{long}/")] {
            let created = File::create(path).map(drop);
            assert_eq!(kind(created), ErrorKind::IsADirectory, "{path}");
        }
        // A rename finds both paths to end in a name, and what it moves
        // to be there, before it looks at the name to move it to.
        assert_eq!(kind(rename("/dir/.", &long)), ErrorKind::ResourceBusy);
        assert_eq!(kind(rename("/missing", &long)), ErrorKind::NotFound);
        assert_eq!(kind(rename("/dir", &long)), ErrorKind::InvalidFilename);

        // What a path does not name cannot be made: the directory is there.
        for path in ["/", "/dir/.", "/dir/.."] {
            assert_eq!(kind(create_dir(path)), ErrorKind::AlreadyExists, "{path}");
        }
        // Only a directory is made by a name with a slash after it, and
        // only a directory is removed by one.
        write("/file", b"x")?;
        let made = [symlink("/file", "/new/"), hard_link("/file", "/new/")];
        for made in made {
            assert_eq!(kind(made), ErrorKind::NotFound);
        }
        assert_eq!(kind(remove_file("/file/")), ErrorKind::NotADirectory);
        create_dir("/new/")?;
        assert_eq!(read("/file")?, b"x");
        Ok(())
    });
    sim.run()
}

/// As with `std::fs`, a path with a NUL in it fails before the system is
/// asked for anything, whatever is wrong with the other path of the call.
#[test]
fn nul_in_a_path_fails_before_any_lookup() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"x")?;
        let refused = [
            rename("/missing/x", "/nu\0l"),
            rename("/file/x", "/nu\0l"),
            hard_link("/missing", "/nu\0l"),
            symlink("", "/nu\0l"),
            symlink("t".repeat(5000), "/nu\0l"),
            copy("/file", "/nu\0l").map(drop),
        ];
        for refused in refused {
            let refused = refused.unwrap_err();
            let told = (refused.kind(), refused.raw_os_error());
            assert_eq!(told, (ErrorKind::InvalidInput, None));
        }
        Ok(())
    });
    sim.run()
}
