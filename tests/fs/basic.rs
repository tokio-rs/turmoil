//! Basic file operation tests.

use std::os::unix::fs::FileExt;
use turmoil::fs::shim::std::fs::{
    copy, create_dir, create_dir_all, read, read_to_string, write, OpenOptions,
};
use turmoil::{Builder, Result};

const TEST_PATH: &str = "/test/data.db";

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
