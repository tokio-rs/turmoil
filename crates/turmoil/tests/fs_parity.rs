//! Compile-only parity check between the filesystem shims and the real
//! `std::fs`, `std::os::unix::fs` and `tokio::fs`.
//!
//! Run `RUSTFLAGS=--cfg=parity_fs cargo check -p turmoil --features
//! unstable-fs --tests` to compile this file against the real modules.
//! What it says about them then holds, and compiling it the usual way
//! holds the shims to the same: code written against the real API builds
//! against the shim with nothing but its imports changed.
//!
//! Nothing here runs. To pin more of the surface, add a line.
//!
//! Left out, because the shims have none of it: file locks, ownership
//! (`chown`), descriptors that are owned or borrowed (`OwnedFd`, `AsFd`),
//! which stand for a file of the kernel's, and I/O through an `Arc<File>`,
//! which only `std` itself can implement for its `File`.

#![cfg(all(unix, feature = "unstable-fs"))]
#![allow(dead_code, unused_imports)]

#[cfg(parity_fs)]
use std::fs;
#[cfg(parity_fs)]
use std::os::unix::fs as unix_fs;
#[cfg(parity_fs)]
use tokio::fs as tokio_fs;
#[cfg(not(parity_fs))]
use turmoil::fs::shim::std::fs;
#[cfg(not(parity_fs))]
use turmoil::fs::shim::std::os::unix::fs as unix_fs;
#[cfg(not(parity_fs))]
use turmoil::fs::shim::tokio::fs as tokio_fs;

use std::ffi::OsString;
use std::fmt::Debug;
use std::hash::Hash;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, DirEntryExt, FileExt, FileTypeExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite};

fn free_functions(path: &Path, perm: fs::Permissions) -> io::Result<()> {
    let _: PathBuf = fs::canonicalize(path)?;
    let _: u64 = fs::copy(path, path)?;
    let _: () = fs::create_dir(path)?;
    let _: () = fs::create_dir_all(path)?;
    let _: bool = fs::exists(path)?;
    let _: () = fs::hard_link(path, path)?;
    let _: fs::Metadata = fs::metadata(path)?;
    let _: Vec<u8> = fs::read(path)?;
    let _: fs::ReadDir = fs::read_dir(path)?;
    let _: PathBuf = fs::read_link(path)?;
    let _: String = fs::read_to_string(path)?;
    let _: () = fs::remove_dir(path)?;
    let _: () = fs::remove_dir_all(path)?;
    let _: () = fs::remove_file(path)?;
    let _: () = fs::rename(path, path)?;
    let _: () = fs::set_permissions(path, perm)?;
    let _: fs::Metadata = fs::symlink_metadata(path)?;
    let _: () = fs::write(path, b"bytes")?;
    let _: () = unix_fs::symlink(path, path)?;
    Ok(())
}

fn file(path: &Path, perm: fs::Permissions, time: SystemTime) -> io::Result<()> {
    let file: fs::File = fs::File::open(path)?;
    let _: fs::File = fs::File::create(path)?;
    let _: fs::File = fs::File::create_new(path)?;
    let _: fs::OpenOptions = fs::File::options();
    let _: () = file.sync_all()?;
    let _: () = file.sync_data()?;
    let _: () = file.set_len(0)?;
    let _: fs::Metadata = file.metadata()?;
    let _: fs::File = file.try_clone()?;
    let _: () = file.set_permissions(perm)?;
    let _: () = file.set_modified(time)?;
    let times: fs::FileTimes = fs::FileTimes::new();
    let times: fs::FileTimes = times.set_accessed(time).set_modified(time);
    let _: () = file.set_times(times)?;

    let _: usize = file.read_at(&mut [0], 0)?;
    let _: usize = file.write_at(&[0], 0)?;
    let _: () = file.read_exact_at(&mut [0], 0)?;
    let _: () = file.write_all_at(&[0], 0)?;

    // A shared reference reads, writes and seeks as the file does.
    let mut shared: &fs::File = &file;
    let _: usize = shared.read(&mut [0])?;
    let _: usize = shared.write(&[0])?;
    let _: u64 = shared.seek(SeekFrom::Start(0))?;
    let _: () = shared.flush()?;
    Ok(())
}

fn open_options(path: &Path) -> io::Result<fs::File> {
    let mut options: fs::OpenOptions = fs::OpenOptions::new();
    options.read(true).write(true).append(true);
    options.truncate(true).create(true).create_new(true);
    options.mode(0o644).custom_flags(0);
    options.clone().open(path)
}

fn metadata(meta: &fs::Metadata) -> io::Result<()> {
    let _: fs::FileType = meta.file_type();
    let _: (bool, bool, bool) = (meta.is_dir(), meta.is_file(), meta.is_symlink());
    let _: u64 = meta.len();
    let _: fs::Permissions = meta.permissions();
    let _: [SystemTime; 3] = [meta.modified()?, meta.accessed()?, meta.created()?];
    let _: [u64; 4] = [meta.dev(), meta.ino(), meta.nlink(), meta.rdev()];
    let _: [u64; 3] = [meta.size(), meta.blksize(), meta.blocks()];
    let _: [u32; 3] = [meta.mode(), meta.uid(), meta.gid()];
    let _: [i64; 3] = [meta.atime(), meta.mtime(), meta.ctime()];
    let _: [i64; 3] = [meta.atime_nsec(), meta.mtime_nsec(), meta.ctime_nsec()];
    let _: fs::Metadata = meta.clone();
    Ok(())
}

fn permissions(mut perm: fs::Permissions, kind: fs::FileType) {
    let _: bool = perm.readonly();
    let _: () = perm.set_readonly(true);
    let _: u32 = perm.mode();
    let _: () = perm.set_mode(0o644);
    let _: bool = perm == fs::Permissions::from_mode(0o644);
    let _: [bool; 3] = [kind.is_dir(), kind.is_file(), kind.is_symlink()];
    let _: [bool; 2] = [kind.is_block_device(), kind.is_char_device()];
    let _: [bool; 2] = [kind.is_fifo(), kind.is_socket()];
}

fn directories(path: &Path) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry: fs::DirEntry = entry?;
        let _: (PathBuf, OsString, u64) = (entry.path(), entry.file_name(), entry.ino());
        let _: fs::Metadata = entry.metadata()?;
        let _: fs::FileType = entry.file_type()?;
    }
    let mut builder: fs::DirBuilder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o755).create(path)
}

fn traits() {
    fn io<T: Read + Write + Seek>() {}
    fn stream<T: AsyncRead + AsyncWrite + AsyncSeek + Unpin>() {}
    fn descriptor<T: AsRawFd>() {}
    fn value<T: Debug>() {}
    fn clone<T: Clone>() {}
    fn key<T: Copy + Eq + Hash + Debug>() {}

    io::<fs::File>();
    io::<&fs::File>();
    stream::<tokio_fs::File>();
    descriptor::<fs::File>();
    descriptor::<tokio_fs::File>();

    value::<(fs::File, fs::OpenOptions, fs::Metadata, fs::ReadDir)>();
    value::<(fs::DirEntry, fs::DirBuilder, fs::Permissions)>();
    value::<(fs::FileTimes, tokio_fs::File, tokio_fs::FileTimes)>();
    value::<(tokio_fs::OpenOptions, tokio_fs::DirBuilder)>();
    value::<(tokio_fs::ReadDir, tokio_fs::DirEntry)>();
    clone::<(fs::OpenOptions, fs::Metadata, fs::Permissions)>();
    clone::<(fs::FileTimes, tokio_fs::OpenOptions)>();
    clone::<tokio_fs::FileTimes>();
    key::<fs::FileType>();
}

async fn tokio_functions(path: &Path, perm: fs::Permissions) -> io::Result<()> {
    let _: PathBuf = tokio_fs::canonicalize(path).await?;
    let _: u64 = tokio_fs::copy(path, path).await?;
    let _: () = tokio_fs::create_dir(path).await?;
    let _: () = tokio_fs::create_dir_all(path).await?;
    let _: () = tokio_fs::hard_link(path, path).await?;
    let _: fs::Metadata = tokio_fs::metadata(path).await?;
    let _: Vec<u8> = tokio_fs::read(path).await?;
    let _: PathBuf = tokio_fs::read_link(path).await?;
    let _: String = tokio_fs::read_to_string(path).await?;
    let _: () = tokio_fs::remove_dir(path).await?;
    let _: () = tokio_fs::remove_dir_all(path).await?;
    let _: () = tokio_fs::remove_file(path).await?;
    let _: () = tokio_fs::rename(path, path).await?;
    let _: () = tokio_fs::set_permissions(path, perm).await?;
    let _: () = tokio_fs::symlink(path, path).await?;
    let _: fs::Metadata = tokio_fs::symlink_metadata(path).await?;
    let _: bool = tokio_fs::try_exists(path).await?;
    let _: () = tokio_fs::write(path, b"bytes").await?;

    let mut entries: tokio_fs::ReadDir = tokio_fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let entry: tokio_fs::DirEntry = entry;
        let _: (PathBuf, OsString, u64) = (entry.path(), entry.file_name(), entry.ino());
        let _: fs::Metadata = entry.metadata().await?;
        let _: fs::FileType = entry.file_type().await?;
    }
    let mut builder: tokio_fs::DirBuilder = tokio_fs::DirBuilder::new();
    builder.recursive(true).mode(0o755).create(path).await
}

async fn tokio_file(path: &Path, perm: fs::Permissions, time: SystemTime) -> io::Result<()> {
    let mut file: tokio_fs::File = tokio_fs::File::open(path).await?;
    let _: tokio_fs::File = tokio_fs::File::create(path).await?;
    let _: tokio_fs::File = tokio_fs::File::create_new(path).await?;
    let _: () = file.sync_all().await?;
    let _: () = file.sync_data().await?;
    let _: () = file.set_len(0).await?;
    let _: fs::Metadata = file.metadata().await?;
    let _: () = file.set_permissions(perm).await?;
    let _: () = file.set_modified(time).await?;
    let times: tokio_fs::FileTimes = tokio_fs::FileTimes::new();
    let times: tokio_fs::FileTimes = times.set_accessed(time).set_modified(time);
    let _: () = file.set_times(times).await?;
    let _: () = file.set_max_buf_size(4096);
    let _: usize = file.max_buf_size();
    let clone: tokio_fs::File = file.try_clone().await?;

    let std: fs::File = clone.into_std().await;
    let back: tokio_fs::File = tokio_fs::File::from_std(std);
    let std: Result<fs::File, tokio_fs::File> = back.try_into_std();
    let _: Option<tokio_fs::File> = std.ok().map(tokio_fs::File::from);

    let mut options: tokio_fs::OpenOptions = tokio_fs::File::options();
    options.read(true).write(true).append(true);
    options.truncate(true).create(true).create_new(true);
    options.mode(0o644).custom_flags(0);
    let _: tokio_fs::File = options.clone().open(path).await?;
    let _: tokio_fs::OpenOptions = fs::OpenOptions::new().into();
    Ok(())
}

/// Every check above is a function that compiles. This one runs, so that
/// the file counts as a test.
#[test]
fn surface_compiles() {
    traits();
}
