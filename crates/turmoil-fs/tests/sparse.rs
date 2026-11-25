use std::io::{ErrorKind, Result};
use std::os::unix::fs::FileExt;
use turmoil_fs::shim::std::fs::{read, sync_dir, File};
use turmoil_fs::Fs;

#[test]
fn read_one_byte_from_a_four_gibibyte_file() -> Result<()> {
    let fs = Fs::builder().capacity(1).build();
    let _guard = fs.enter();
    let file = File::create("/wal")?;
    file.write_all_at(b"x", 0)?;
    file.set_len(1 << 32)?;
    file.sync_data()?;
    sync_dir("/")?;
    fs.crash();

    let file = File::open("/wal")?;
    assert_eq!(file.metadata()?.len(), 1 << 32);
    let mut byte = [0];
    file.read_exact_at(&mut byte, 0)?;
    assert_eq!(&byte, b"x");
    if usize::BITS == 32 {
        assert_eq!(read("/wal").unwrap_err().kind(), ErrorKind::FileTooLarge);
    }
    Ok(())
}
