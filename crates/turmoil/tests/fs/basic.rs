//! Basic file operation tests.

use std::io::{ErrorKind, Seek, SeekFrom};
use std::os::unix::fs::{FileExt, MetadataExt};
use turmoil::fs::shim::std::fs::{
    copy, create_dir, create_dir_all, read, read_to_string, remove_file, rename, write, File,
    OpenOptions,
};
use turmoil::fs::Fs;
use turmoil::{Builder, Result};

const TEST_PATH: &str = "/test/data.db";

fn open_rw(path: &str) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).open(path)
}

#[test]
fn basic_read_write() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(b"hello world", 0)?;
        let mut buf = [0u8; 11];
        let n = file.read_at(&mut buf, 0)?;
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
        Ok(())
    });
    sim.run()
}

#[test]
fn write_at_offset() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(b"at offset", 100)?;
        let mut buf = [0u8; 9];
        file.read_at(&mut buf, 100)?;
        assert_eq!(&buf, b"at offset");
        Ok(())
    });
    sim.run()
}

#[test]
fn set_len_truncate() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(b"0123456789", 0)?;
        file.sync_all()?;
        file.set_len(5)?;
        assert_eq!(file.metadata()?.len(), 5);
        Ok(())
    });
    sim.run()
}

#[test]
fn multiple_files() -> Result {
    let mut sim = Builder::new().build();
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
        f1.write_all_at(b"file one", 0)?;
        f2.write_all_at(b"file two", 0)?;
        let mut b1 = [0u8; 8];
        let mut b2 = [0u8; 8];
        f1.read_at(&mut b1, 0)?;
        f2.read_at(&mut b2, 0)?;
        assert_eq!(&b1, b"file one");
        assert_eq!(&b2, b"file two");
        Ok(())
    });
    sim.run()
}

#[test]
fn read_write_convenience() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/file.txt", b"hello world")?;
        assert_eq!(read("/data/file.txt")?, b"hello world");
        assert_eq!(read_to_string("/data/file.txt")?, "hello world");
        Ok(())
    });
    sim.run()
}

#[test]
fn copy_file() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        write("/data/source.txt", b"copy me")?;
        let bytes = copy("/data/source.txt", "/data/dest.txt")?;
        assert_eq!(bytes, 7);
        assert_eq!(read("/data/dest.txt")?, b"copy me");
        Ok(())
    });
    sim.run()
}

#[test]
fn set_len_extend() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/test/data.db")?;
        file.set_len(100)?;
        assert_eq!(file.metadata()?.len(), 100);
        let mut buf = [0xffu8; 100];
        file.read_at(&mut buf, 0)?;
        assert!(buf.iter().all(|&b| b == 0));
        Ok(())
    });
    sim.run()
}

#[test]
fn open_options_create_new() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let _file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(TEST_PATH)?;
        let result = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(TEST_PATH);
        assert!(result.is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn open_options_truncate() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(b"existing data", 0)?;
        file.sync_all()?;
        drop(file);
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(TEST_PATH)?;
        assert_eq!(file.metadata()?.len(), 0);
        Ok(())
    });
    sim.run()
}

#[test]
fn read_exact_at() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(b"exact read test", 0)?;
        let mut buf = [0u8; 15];
        file.read_exact_at(&mut buf, 0)?;
        assert_eq!(&buf, b"exact read test");
        let mut buf2 = [0u8; 100];
        assert!(file.read_exact_at(&mut buf2, 0).is_err());
        Ok(())
    });
    sim.run()
}

#[test]
fn overlapping_writes() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(TEST_PATH)?;
        file.write_all_at(&[b'A'; 10], 0)?;
        file.write_all_at(&[b'B'; 3], 3)?;
        let mut buf = [0u8; 10];
        file.read_at(&mut buf, 0)?;
        assert_eq!(&buf, b"AAABBBAAAA");
        Ok(())
    });
    sim.run()
}

#[test]
fn cursor_based_io() -> Result {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file.write_all(b"hello world")?;
        file.seek(SeekFrom::Start(0))?;
        let mut buf = [0u8; 11];
        file.read_exact(&mut buf)?;
        assert_eq!(&buf, b"hello world");
        file.seek(SeekFrom::End(-5))?;
        let mut buf2 = [0u8; 5];
        file.read_exact(&mut buf2)?;
        assert_eq!(&buf2, b"world");
        Ok(())
    });
    sim.run()
}

#[test]
fn append_mode() -> Result {
    use std::io::Write;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .open("/data/log.txt")?;
        file.write_all_at(b"line1\n", 0)?;
        file.sync_all()?;
        drop(file);
        let mut file = OpenOptions::new().append(true).open("/data/log.txt")?;
        file.write_all(b"line2\n")?;
        drop(file);
        let file = OpenOptions::new().read(true).open("/data/log.txt")?;
        let mut buf = [0u8; 12];
        file.read_exact_at(&mut buf, 0)?;
        assert_eq!(&buf, b"line1\nline2\n");
        Ok(())
    });
    sim.run()
}

#[test]
fn file_try_clone() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/data")?;
        let file1 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/data/file.txt")?;
        file1.write_all_at(b"hello world", 0)?;
        let file2 = file1.try_clone()?;
        let mut buf = [0u8; 5];
        file2.read_at(&mut buf, 6)?;
        assert_eq!(&buf, b"world");
        file2.write_all_at(b"HELLO", 0)?;
        let mut buf2 = [0u8; 5];
        file1.read_at(&mut buf2, 0)?;
        assert_eq!(&buf2, b"HELLO");
        Ok(())
    });
    sim.run()
}

/// A handle refuses what it was not opened for the way Linux does: a read
/// or a write as a bad descriptor, a truncation as an invalid argument. An
/// offset or a length that is out of range is refused before that.
#[test]
fn handle_refuses_what_it_was_not_opened_for() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"0123456789")?;
        let read_only = File::open("/file")?;
        let write_only = OpenOptions::new().write(true).open("/file")?;
        let appending = OpenOptions::new().append(true).open("/file")?;
        let mut buf = [0; 4];
        let refused = [
            read_only.write_at(b"ab", 0),
            read_only.write_at(b"", 0),
            write_only.read_at(&mut buf, 0),
            write_only.read_at(&mut [], 0),
            appending.read_at(&mut buf, 0),
        ];
        for (index, refused) in refused.into_iter().enumerate() {
            assert_eq!(
                refused.unwrap_err().raw_os_error(),
                Some(libc::EBADF),
                "{index}"
            );
        }
        let invalid = [
            read_only.set_len(4),
            read_only.set_len(u64::MAX),
            write_only.set_len(u64::MAX),
            read_only.read_at(&mut buf, u64::MAX).map(drop),
            write_only.read_at(&mut buf, u64::MAX).map(drop),
            read_only.write_at(b"ab", u64::MAX).map(drop),
            write_only.write_at(b"ab", u64::MAX).map(drop),
        ];
        for (index, invalid) in invalid.into_iter().enumerate() {
            assert_eq!(
                invalid.unwrap_err().kind(),
                ErrorKind::InvalidInput,
                "{index}"
            );
        }
        assert_eq!(read("/file")?, b"0123456789");
        Ok(())
    });
    sim.run()
}

/// A handle opened to append writes at the end of the file wherever it is
/// told to, as on Linux, and its cursor moves only past what it wrote.
#[test]
fn append_handle_writes_at_the_end() -> Result {
    use std::io::Write;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/log", b"0123456789")?;
        let mut log = OpenOptions::new().append(true).open("/log")?;
        assert_eq!(log.write_at(b"ab", 2)?, 2);
        assert_eq!(log.write(b"")?, 0);
        assert_eq!(log.stream_position()?, 0);
        log.seek(SeekFrom::Start(3))?;
        log.write_all(b"cd")?;
        assert_eq!(log.stream_position()?, 14);
        assert_eq!(log.write_at(b"ef", 30)?, 2);
        assert_eq!(read("/log")?, b"0123456789abcdef");
        Ok(())
    });
    sim.run()
}

/// A cloned handle is the same open file: the two share one cursor. A
/// file opened again has a cursor of its own.
#[test]
fn cloned_handle_shares_its_cursor() -> Result {
    use std::io::{Read, Write};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"0123456789")?;
        let mut file = OpenOptions::new().read(true).write(true).open("/file")?;
        let mut clone = file.try_clone()?;
        let mut buf = [0; 2];
        clone.read_exact(&mut buf)?;
        assert_eq!(file.stream_position()?, 2);
        file.seek(SeekFrom::Start(5))?;
        assert_eq!(clone.stream_position()?, 5);
        clone.write_all(b"ab")?;
        file.write_all(b"cd")?;
        assert_eq!(read("/file")?, b"01234abcd9");

        let mut reopened = File::open("/file")?;
        reopened.read_exact(&mut buf)?;
        assert_eq!((&buf, clone.stream_position()?), (b"01", 9));
        Ok(())
    });
    sim.run()
}

/// The options of an open are checked as `std::fs` checks them, before
/// the filesystem is asked: an open needs some access, and write access
/// to create or truncate.
#[test]
fn open_options_are_validated() -> Result {
    type Options = fn(&mut OpenOptions) -> &mut OpenOptions;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        write("/file", b"kept")?;
        let invalid: [Options; 7] = [
            |open| open,
            |open| open.create(true),
            |open| open.read(true).create(true),
            |open| open.read(true).truncate(true),
            |open| open.read(true).create_new(true),
            |open| open.append(true).truncate(true),
            |open| open.write(true).append(true).truncate(true).create(true),
        ];
        for (index, options) in invalid.into_iter().enumerate() {
            for path in ["/file", "/missing", "/"] {
                let refused = options(&mut OpenOptions::new()).open(path).unwrap_err();
                assert_eq!(refused.kind(), ErrorKind::InvalidInput, "{index} {path}");
            }
        }
        assert_eq!(read("/file")?, b"kept");
        assert!(read("/missing").is_err());

        // A new file has nothing to truncate, so appending to one may ask.
        let mut options = OpenOptions::new();
        options.append(true).truncate(true).create_new(true);
        options.open("/new")?.write_all_at(b"ab", 0)?;
        assert_eq!(read("/new")?, b"ab");
        Ok(())
    });
    sim.run()
}

/// A copy takes the permissions of its source, as `std::fs::copy` gives
/// it on Linux, and its destination is emptied before anything is read.
#[test]
fn copy_takes_permissions_and_empties_first() -> Result {
    use turmoil::fs::shim::std::fs::{metadata, set_permissions, Permissions};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mode = |path: &str| metadata(path).map(|meta| meta.permissions().mode() & 0o7777);
        write("/source", b"abc")?;
        set_permissions("/source", Permissions::from_mode(0o600))?;
        write("/loud", b"0123456789")?;
        set_permissions("/loud", Permissions::from_mode(0o777))?;

        for copied in ["/new", "/loud"] {
            assert_eq!(copy("/source", copied)?, 3);
            assert_eq!((read(copied)?, mode(copied)?), (b"abc".to_vec(), 0o600));
        }
        assert_eq!(copy("/source", "/source")?, 0);
        assert_eq!(read("/source")?, b"");
        Ok(())
    });
    sim.run()
}

#[test]
fn per_host_isolation() -> Result {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let mut sim = Builder::new().build();
    let n1 = Arc::new(Notify::new());
    let n2 = Arc::new(Notify::new());
    let n1h = n1.clone();
    sim.host("host1", move || {
        let n = n1h.clone();
        async move {
            create_dir_all("/shared")?;
            write("/shared/data", b"from host1")?;
            n.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n2h = n2.clone();
    sim.host("host2", move || {
        let n = n2h.clone();
        async move {
            create_dir_all("/shared")?;
            write("/shared/data", b"from host2")?;
            assert_eq!(read("/shared/data")?, b"from host2");
            n.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    sim.client("test", async move {
        n1.notified().await;
        n2.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn as_raw_fd_yields_distinct_high_range_values() -> Result {
    use std::os::fd::AsRawFd;
    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/test")?;
        let f1 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/test/a")?;
        let f2 = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open("/test/b")?;
        // Distinct fds.
        assert_ne!(f1.as_raw_fd(), f2.as_raw_fd());
        // Both above SIM_FD_BASE, so they cannot collide with real OS fds.
        assert!(
            f1.as_raw_fd() >= 1 << 30,
            "fd {} below SIM_FD_BASE",
            f1.as_raw_fd()
        );
        assert!(
            f2.as_raw_fd() >= 1 << 30,
            "fd {} below SIM_FD_BASE",
            f2.as_raw_fd()
        );
        Ok(())
    });
    sim.run()
}

#[test]
fn as_raw_fd_isolated_per_host() -> Result {
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use tokio::sync::Notify;
    let mut sim = Builder::new().build();
    // Each host opens one file; both expect the same fd integer because
    // the fd table is per-host (two independent counters starting at
    // SIM_FD_BASE).
    let n1 = Arc::new(Notify::new());
    let n2 = Arc::new(Notify::new());
    let n1h = n1.clone();
    sim.host("host1", move || {
        let n = n1h.clone();
        async move {
            create_dir_all("/h")?;
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/h/f")?;
            assert_eq!(f.as_raw_fd(), 1 << 30);
            n.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    let n2h = n2.clone();
    sim.host("host2", move || {
        let n = n2h.clone();
        async move {
            create_dir_all("/h")?;
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open("/h/f")?;
            assert_eq!(f.as_raw_fd(), 1 << 30);
            n.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    });
    sim.client("test", async move {
        n1.notified().await;
        n2.notified().await;
        Ok(())
    });
    sim.run()
}

#[test]
fn truncate_then_extend_reads_zeros() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let file = open_rw("/file")?;
        file.write_all_at(b"hello world", 0)?;
        file.set_len(0)?;
        file.set_len(11)?;
        let mut buf = [0xffu8; 11];
        file.read_exact_at(&mut buf, 0)?;
        assert_eq!(buf, [0u8; 11]);
        Ok(())
    });
    sim.run()
}

#[test]
fn open_file_follows_rename() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let file = open_rw("/old")?;
        file.write_all_at(b"abc", 0)?;
        rename("/old", "/new")?;

        file.write_all_at(b"def", 3)?;
        file.sync_all()?;
        assert_eq!(read("/new")?, b"abcdef");

        let renamed = OpenOptions::new().write(true).open("/new")?;
        renamed.write_all_at(b"ghi", 6)?;
        let mut buf = [0u8; 9];
        file.read_exact_at(&mut buf, 0)?;
        assert_eq!(&buf, b"abcdefghi");
        Ok(())
    });
    sim.run()
}

#[test]
fn removed_file_lives_on_through_open_handle() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let old = open_rw("/file")?;
        old.write_all_at(b"old", 0)?;
        remove_file("/file")?;

        let new = OpenOptions::new().write(true).create(true).open("/file")?;
        new.write_all_at(b"N", 0)?;

        let mut buf = [0u8; 3];
        old.read_exact_at(&mut buf, 0)?;
        assert_eq!(&buf, b"old");
        assert_eq!(old.metadata()?.nlink(), 0);
        assert_eq!(read("/file")?, b"N");
        Ok(())
    });
    sim.run()
}

#[test]
fn preallocated_space_reads_as_zeros() -> Result {
    let mut sim = Builder::new().build();
    sim.client("test", async {
        let file = open_rw("/wal")?;
        // Far more than fits in memory, were it stored.
        file.set_len(1 << 40)?;
        file.sync_all()?;
        assert_eq!(file.metadata()?.blocks(), 0);
        file.write_all_at(b"head", 0)?;
        file.write_all_at(b"x", 4096)?;
        assert_eq!(file.metadata()?.blocks(), 2);
        let mut tail = [0xffu8; 8];
        file.read_exact_at(&mut tail, (1 << 40) - 8)?;
        assert_eq!(tail, [0u8; 8]);
        assert_eq!(file.metadata()?.len(), 1 << 40);
        Ok(())
    });
    sim.run()
}

/// A write far into a file stores what was written, not the hole before it.
#[test]
fn write_at_the_end_of_a_sparse_file() -> Result {
    use std::io::Write;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mut file = open_rw("/wal")?;
        file.set_len(1 << 40)?;
        file.write_all_at(b"tail", (1 << 40) - 4)?;
        file.sync_all()?;
        let mut tail = [0xffu8; 8];
        file.read_exact_at(&mut tail, (1 << 40) - 8)?;
        assert_eq!(&tail, b"\0\0\0\0tail");

        let max_len = i64::MAX as u64;
        file.write_all_at(b"x", max_len - 1)?;
        let mut byte = [0u8; 1];
        file.read_exact_at(&mut byte, max_len - 1)?;
        assert_eq!(&byte, b"x");
        assert_eq!(file.seek(SeekFrom::End(0))?, max_len);
        let invalid = file.seek(SeekFrom::Current(1)).unwrap_err();
        assert_eq!(invalid.kind(), ErrorKind::InvalidInput);
        for len in [max_len + 1, u64::MAX] {
            let invalid = file.set_len(len).unwrap_err();
            assert_eq!(invalid.kind(), ErrorKind::InvalidInput);
        }
        // As on Linux, a transfer that would end past the largest offset is
        // no valid one, wherever it starts and whatever is there.
        let past = [
            file.write_at(b"x", max_len),
            file.write_at(b"xy", max_len - 1),
            file.read_at(&mut [0; 4], max_len - 3),
            file.write(b"x"),
        ];
        for past in past {
            assert_eq!(past.unwrap_err().kind(), ErrorKind::InvalidInput);
        }
        assert_eq!(file.read_at(&mut [0; 4], max_len - 4)?, 4);
        // A file as long as files get has no room for what is appended.
        let mut log = OpenOptions::new().append(true).open("/wal")?;
        let full = log.write(b"x").unwrap_err();
        assert_eq!(full.kind(), ErrorKind::FileTooLarge);
        assert_eq!(file.stream_position()?, max_len);
        assert_eq!(file.metadata()?.len(), max_len);
        Ok(())
    });
    sim.run()
}

/// A file belongs to the filesystem it was opened on. On another one its
/// descriptor names nothing, rather than whatever has the same number there.
#[test]
fn file_does_not_reach_into_another_filesystem() -> std::io::Result<()> {
    let (ours, theirs) = (Fs::builder().build(), Fs::builder().build());
    let on_ours = ours.enter();
    let (mut file, other) = (open_rw("/file")?, open_rw("/other")?);
    file.write_all_at(b"ours", 0)?;
    drop(on_ours);

    // The first descriptor has the same number on either filesystem.
    let on_theirs = theirs.enter();
    let same_number = open_rw("/file")?;
    same_number.write_all_at(b"theirs", 0)?;
    let stale = [
        file.read_at(&mut [0; 4], 0).map(drop),
        file.write_at(b"lost", 0).map(drop),
        file.set_len(0),
        file.sync_all(),
        file.try_clone().map(drop),
        file.seek(SeekFrom::Start(2)).map(drop),
        file.seek(SeekFrom::Current(1)).map(drop),
    ];
    for result in stale {
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
    }
    assert_eq!(read("/file")?, b"theirs");
    drop(on_theirs);

    let _on_ours = ours.enter();
    assert_eq!(file.stream_position()?, 0);
    assert_eq!(read("/file")?, b"ours");
    assert_eq!(other.metadata()?.len(), 0);
    ours.close_all();
    for pos in [SeekFrom::Start(1), SeekFrom::Current(1), SeekFrom::End(0)] {
        assert_eq!(file.seek(pos).unwrap_err().kind(), ErrorKind::NotFound);
    }
    Ok(())
}

/// `File::create_new` makes a file to read and write, where there is none.
/// `File::options` is a blank set of options.
#[test]
fn create_new_reads_and_writes_a_new_file() -> Result {
    use std::io::{Read, Write};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mut file = File::create_new("/file")?;
        file.write_all(b"new")?;
        file.seek(SeekFrom::Start(0))?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        assert_eq!(content, "new");
        let taken = File::create_new("/file").unwrap_err();
        assert_eq!(taken.kind(), ErrorKind::AlreadyExists);

        let blank = File::options().open("/file").unwrap_err();
        assert_eq!(blank.kind(), ErrorKind::InvalidInput);
        let reader = File::options().read(true).open("/file")?;
        assert_eq!(reader.metadata()?.len(), 3);
        Ok(())
    });
    sim.run()
}

/// A shared reference to a file reads, writes and seeks, and moves the
/// cursor of the file as it does.
#[test]
fn shared_reference_reads_writes_and_seeks() -> Result {
    use std::io::{Read, Write};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        let mut file = File::create_new("/file")?;
        let mut shared = &file;
        shared.write_all(b"0123456789")?;
        shared.flush()?;
        assert_eq!(shared.seek(SeekFrom::Start(4))?, 4);
        let mut buf = [0; 2];
        shared.read_exact(&mut buf)?;
        assert_eq!((&buf, file.stream_position()?), (b"45", 6));
        Ok(())
    });
    sim.run()
}

/// What the filesystem fails with is an error of the system, with the
/// number the host has for it, as an error of `std::fs` is. What `std::fs`
/// refuses on its own has no number.
#[test]
fn errors_have_the_number_of_the_host() -> Result {
    use turmoil::fs::shim::std::fs::{hard_link, remove_dir};

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir_all("/dir/sub")?;
        write("/file", b"x")?;
        let failures = [
            (read("/missing").map(drop), libc::ENOENT),
            (create_dir("/dir"), libc::EEXIST),
            (read("/file/x").map(drop), libc::ENOTDIR),
            (write("/dir", b"x"), libc::EISDIR),
            (remove_dir("/dir"), libc::ENOTEMPTY),
            (rename("/dir", "/dir/sub/in"), libc::EINVAL),
            (remove_dir("/"), libc::EBUSY),
            (hard_link("/dir", "/twin"), libc::EPERM),
        ];
        for (failure, errno) in failures {
            assert_eq!(failure.unwrap_err().raw_os_error(), Some(errno));
        }
        let own = [
            write("/nu\0l", b"x"),
            File::open("/file")?.set_len(u64::MAX),
            OpenOptions::new().open("/file").map(drop),
        ];
        for own in own {
            assert_eq!(own.unwrap_err().raw_os_error(), None);
        }
        Ok(())
    });
    sim.run()
}

/// Custom flags are the flags of `open(2)`, and do what they do there.
#[test]
fn custom_flags_are_those_of_open() -> Result {
    use libc::{O_APPEND, O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_TRUNC};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use turmoil::fs::shim::std::os::unix::fs::symlink;

    let mut sim = Builder::new().build();
    sim.client("test", async {
        create_dir("/dir")?;
        write("/file", b"0123456789")?;
        symlink("/file", "/link")?;
        symlink("/dir", "/to_dir")?;
        let open = |path: &str, flags: i32, write: bool| {
            let mut options = OpenOptions::new();
            options.read(true).write(write);
            options.custom_flags(flags).open(path)
        };
        let refusals = [
            ("/file", O_DIRECTORY, libc::ENOTDIR),
            ("/link", O_NOFOLLOW, libc::ELOOP),
            ("/link", O_NOFOLLOW | O_DIRECTORY, libc::ENOTDIR),
            ("/dir", O_TRUNC, libc::EISDIR),
            ("/missing", O_CREAT | O_DIRECTORY, libc::EINVAL),
            ("/file", O_CREAT | O_EXCL, libc::EEXIST),
        ];
        for (path, flags, errno) in refusals {
            let refused = open(path, flags, false).unwrap_err();
            assert_eq!(refused.raw_os_error(), Some(errno), "{path} {flags:#x}");
        }
        open("/dir", O_DIRECTORY, false)?;
        open("/to_dir/", O_NOFOLLOW, false)?;

        open("/file", O_APPEND, true)?.write_all(b"ab")?;
        assert_eq!(read("/file")?, b"0123456789ab");
        open("/file", O_TRUNC, false)?;
        open("/made", O_CREAT, true)?;
        assert_eq!((read("/file")?, read("/made")?), (vec![], vec![]));

        // Only a regular file opens for direct I/O.
        let mut options = OpenOptions::new();
        let direct = options.read(true).direct_io(true).open("/dir");
        assert_eq!(direct.unwrap_err().raw_os_error(), Some(libc::EINVAL));
        Ok(())
    });
    sim.run()
}

#[test]
fn file_handle_can_outlive_the_host() -> std::io::Result<()> {
    use std::sync::{Arc, Mutex};
    use turmoil::fs::{enter, FsState};

    let state = Arc::new(Mutex::new(FsState::default()));
    let owner = Arc::downgrade(&state);
    let file = {
        let _entered = enter(&state);
        File::create("/file")?
    };
    drop(state);
    assert!(owner.upgrade().is_none());
    drop(file);
    Ok(())
}
