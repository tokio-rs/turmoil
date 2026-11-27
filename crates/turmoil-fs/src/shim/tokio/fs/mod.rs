//! Async filesystem shims mirroring `tokio::fs`.
//!
//! Provides async wrappers around the [`std::fs`](crate::shim::std::fs) shims.
//!
//! # I/O Latency
//!
//! When I/O latency is configured via [`crate::FsConfig::io_latency`], these
//! async operations will sleep for the configured duration using simulated time.
//! This allows testing code that is sensitive to I/O latency without slowing down
//! tests.
//!
//! Note: Only async operations (this module) have latency. Sync operations via
//! [`crate::shim::std::fs`] complete immediately.
//!
//! # Usage
//!
//! ```ignore
//! use turmoil::fs::shim::tokio::fs;
//!
//! async fn example() -> std::io::Result<()> {
//!     fs::create_dir_all("/data").await?;
//!     fs::write("/data/file.txt", b"hello").await?;
//!     let contents = fs::read("/data/file.txt").await?;
//!     Ok(())
//! }
//! ```

use crate::shim::std::fs as sync_fs;
use crate::shim::std::os::unix::fs as sync_unix_fs;
use crate::{Access, FsContext, FsState, Written};
use std::ffi::OsString;
use std::io::{Read, Result, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, DirEntryExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite, ReadBuf};

async fn delay(latency: Duration) {
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
}

/// Apply I/O latency if configured (assumes cache miss).
async fn apply_io_latency() {
    delay(FsContext::current(|ctx| ctx.fs.calculate_latency(false))).await;
}

// The types `tokio::fs` takes from `std::fs`.
pub use sync_fs::{FileType, Metadata, Permissions};

/// Largest read or write one call makes, as with `tokio::fs::File`.
const DEFAULT_MAX_BUF_SIZE: usize = 2 * 1024 * 1024;

/// Async version of [`std::fs::canonicalize`].
pub async fn canonicalize<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    sync_fs::canonicalize(path)
}

/// Async version of [`std::fs::copy`].
///
/// This operation incurs I/O latency if configured.
pub async fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<u64> {
    let result = sync_fs::copy(&from, &to);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::create_dir`].
pub async fn create_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    sync_fs::create_dir(path)
}

/// Async version of [`std::fs::create_dir_all`].
pub async fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    sync_fs::create_dir_all(path)
}

/// Async version of [`std::fs::hard_link`].
pub async fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    sync_fs::hard_link(original, link)
}

/// Async version of [`std::fs::metadata`].
///
/// This operation incurs I/O latency if configured.
pub async fn metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    let result = sync_fs::metadata(&path);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::read`].
///
/// This operation incurs I/O latency if configured.
pub async fn read<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    let result = sync_fs::read(&path);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::read_dir`].
///
/// This operation incurs I/O latency if configured.
pub async fn read_dir<P: AsRef<Path>>(path: P) -> Result<ReadDir> {
    let result = sync_fs::read_dir(&path);
    apply_io_latency().await;
    result.map(ReadDir)
}

/// Reads the entries in a directory.
///
/// Returned by [`read_dir`]. Drop-in replacement for `tokio::fs::ReadDir`.
#[derive(Debug)]
pub struct ReadDir(sync_fs::ReadDir);

impl ReadDir {
    /// Returns the next entry in the directory stream.
    pub async fn next_entry(&mut self) -> Result<Option<DirEntry>> {
        let entry = self.0.next().transpose()?;
        Ok(entry.map(DirEntry))
    }

    /// Polls for the next directory entry in the stream.
    pub fn poll_next_entry(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Option<DirEntry>>> {
        let entry = self.0.next().transpose();
        Poll::Ready(entry.map(|entry| entry.map(DirEntry)))
    }
}

/// Entries returned by the [`ReadDir`] stream.
///
/// Drop-in replacement for `tokio::fs::DirEntry`.
#[derive(Debug)]
pub struct DirEntry(sync_fs::DirEntry);

impl DirEntry {
    /// Returns the underlying `d_ino` field in the contained `dirent`.
    pub fn ino(&self) -> u64 {
        self.0.ino()
    }

    /// Returns the full path to the file that this entry represents.
    pub fn path(&self) -> PathBuf {
        self.0.path()
    }

    /// Returns the bare file name of this directory entry.
    pub fn file_name(&self) -> OsString {
        self.0.file_name()
    }

    /// Returns the metadata for the file that this entry points at, without
    /// following symlinks.
    ///
    /// This operation incurs I/O latency if configured.
    pub async fn metadata(&self) -> Result<Metadata> {
        let result = self.0.metadata();
        apply_io_latency().await;
        result
    }

    /// Returns the file type for the file that this entry points at.
    pub async fn file_type(&self) -> Result<FileType> {
        self.0.file_type()
    }
}

/// Async version of [`std::fs::read_link`].
///
/// This operation incurs I/O latency if configured.
pub async fn read_link<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let result = sync_fs::read_link(&path);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::read_to_string`].
///
/// This operation incurs I/O latency if configured.
pub async fn read_to_string<P: AsRef<Path>>(path: P) -> Result<String> {
    let result = sync_fs::read_to_string(&path);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::remove_dir`].
pub async fn remove_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    sync_fs::remove_dir(path)
}

/// Async version of [`std::fs::remove_dir_all`].
pub async fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    sync_fs::remove_dir_all(path)
}

/// Async version of [`std::fs::remove_file`].
pub async fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    sync_fs::remove_file(path)
}

/// Async version of [`std::fs::rename`].
pub async fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    sync_fs::rename(from, to)
}

/// Async version of [`std::fs::set_permissions`].
pub async fn set_permissions<P: AsRef<Path>>(path: P, perm: Permissions) -> Result<()> {
    sync_fs::set_permissions(path, perm)
}

/// Async version of creating a symlink.
pub async fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    sync_unix_fs::symlink(original, link)
}

/// Async version of [`std::fs::symlink_metadata`].
///
/// This operation incurs I/O latency if configured.
pub async fn symlink_metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    let result = sync_fs::symlink_metadata(&path);
    apply_io_latency().await;
    result
}

/// Async version of [`std::fs::write`].
///
/// This operation incurs I/O latency if configured.
pub async fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
    let result = sync_fs::write(&path, &contents);
    apply_io_latency().await;
    result
}

/// Returns `Ok(true)` if the path exists, and `Ok(false)` if it does not.
/// Whatever else keeps the path from being looked up is an error.
pub async fn try_exists<P: AsRef<Path>>(path: P) -> Result<bool> {
    sync_fs::exists(path)
}

/// Syncs a directory (turmoil-specific, not in tokio::fs).
///
/// See [`crate::shim::std::fs::sync_dir`] for details.
///
/// This operation incurs I/O latency if configured.
pub async fn sync_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let result = sync_fs::sync_dir(&path);
    apply_io_latency().await;
    result
}

/// Timestamps for [`File::set_times`].
///
/// Drop-in replacement for `tokio::fs::FileTimes`, a type of its own beside
/// the one in `std::fs`.
#[derive(Debug, Clone, Default)]
#[must_use = "must be applied to a file via `File::set_times` to have any effect"]
pub struct FileTimes(sync_fs::FileTimes);

impl FileTimes {
    /// Creates a new empty `FileTimes`. Unset timestamps are left unchanged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the last access time.
    pub fn set_accessed(self, t: SystemTime) -> Self {
        Self(self.0.set_accessed(t))
    }

    /// Sets the last modification time.
    pub fn set_modified(self, t: SystemTime) -> Self {
        Self(self.0.set_modified(t))
    }
}

/// An async file handle.
///
/// This is the async equivalent of [`std::fs::File`](crate::shim::std::fs::File).
/// It implements [`AsyncRead`], [`AsyncWrite`] and [`AsyncSeek`].
#[derive(Debug)]
pub struct File {
    inner: sync_fs::File,
    /// Largest read or write one call makes.
    max_buf: usize,
}

impl From<sync_fs::File> for File {
    fn from(std: sync_fs::File) -> Self {
        Self::from_std(std)
    }
}

impl File {
    /// Opens a file in read-only mode.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<File> {
        sync_fs::File::open(path).map(File::from_std)
    }

    /// Opens a file in write-only mode, creating it if it doesn't exist.
    pub async fn create<P: AsRef<Path>>(path: P) -> Result<File> {
        sync_fs::File::create(path).map(File::from_std)
    }

    /// Opens a file in read-write mode, and fails if it exists.
    pub async fn create_new<P: AsRef<Path>>(path: P) -> Result<File> {
        sync_fs::File::create_new(path).map(File::from_std)
    }

    /// Returns a new [`OpenOptions`] object.
    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    /// Converts a [`std::fs::File`](crate::shim::std::fs::File) to an async `File`.
    pub fn from_std(inner: sync_fs::File) -> File {
        let max_buf = DEFAULT_MAX_BUF_SIZE;
        File { inner, max_buf }
    }

    /// Converts this async `File` into a sync [`std::fs::File`](crate::shim::std::fs::File).
    pub async fn into_std(self) -> sync_fs::File {
        self.inner
    }

    /// Tries to convert the file at once, which always works here: no
    /// operation is ever in flight between two calls.
    pub fn try_into_std(self) -> std::result::Result<sync_fs::File, File> {
        Ok(self.inner)
    }

    /// Sets the largest read or write one call makes.
    pub fn set_max_buf_size(&mut self, max_buf_size: usize) {
        self.max_buf = max_buf_size;
    }

    /// Returns the largest read or write one call makes.
    pub fn max_buf_size(&self) -> usize {
        self.max_buf
    }

    /// Changes the permissions of the file.
    pub async fn set_permissions(&self, perm: Permissions) -> Result<()> {
        self.inner.set_permissions(perm)
    }

    /// Changes the timestamps of the file.
    pub async fn set_times(&self, times: FileTimes) -> Result<()> {
        self.inner.set_times(times.0)
    }

    /// Changes the modification time of the file.
    pub async fn set_modified(&self, time: SystemTime) -> Result<()> {
        self.inner.set_modified(time)
    }

    /// Attempts to sync all OS-internal metadata and data to disk.
    ///
    /// This operation incurs I/O latency if configured.
    pub async fn sync_all(&self) -> Result<()> {
        let result = self.inner.sync_all();
        apply_io_latency().await;
        result
    }

    /// Attempts to sync file data to disk.
    ///
    /// This operation incurs I/O latency if configured.
    pub async fn sync_data(&self) -> Result<()> {
        let result = self.inner.sync_data();
        apply_io_latency().await;
        result
    }

    /// Truncates or extends the file to the specified size.
    ///
    /// This operation incurs I/O latency if configured.
    pub async fn set_len(&self, size: u64) -> Result<()> {
        let result = self.inner.set_len(size);
        apply_io_latency().await;
        result
    }

    /// Queries metadata about the file.
    ///
    /// This operation incurs I/O latency if configured.
    pub async fn metadata(&self) -> Result<Metadata> {
        let result = self.inner.metadata();
        apply_io_latency().await;
        result
    }

    /// Creates a new handle that shares the underlying open file.
    pub async fn try_clone(&self) -> Result<File> {
        let (inner, max_buf) = (self.inner.try_clone()?, self.max_buf);
        Ok(File { inner, max_buf })
    }

    /// Reads bytes from the file at the specified offset.
    ///
    /// This operation incurs I/O latency if configured. If page cache is enabled,
    /// cache hits result in near-instant reads.
    pub async fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let read = |fs: &mut FsState, fd| {
            let len = fs.read_at(fd, buf, offset)?;
            Ok(Written { len, offset })
        };
        self.transfer(Access::Read, read).await
    }

    /// Writes bytes to the file at the specified offset.
    ///
    /// This operation incurs I/O latency if configured. Writes populate
    /// the page cache for subsequent reads.
    pub async fn write_at(&self, buf: &[u8], offset: u64) -> Result<usize> {
        let write = |fs: &mut FsState, fd| fs.write_at(fd, buf, offset);
        self.transfer(Access::Write, write).await
    }

    /// Runs a transfer, then takes the time that it would on a disk: by
    /// where in the file the bytes came from or went.
    async fn transfer(
        &self,
        access: Access,
        op: impl FnOnce(&mut FsState, RawFd) -> Result<Written>,
    ) -> Result<usize> {
        let fd = self.as_raw_fd();
        let (result, latency) = self.inner.context(|ctx| {
            let result = op(ctx.fs, fd);
            let latency = match (&result, access) {
                (Ok(Written { len: 1.., offset }), Access::Read) => {
                    ctx.fs.read_latency(fd, *offset)
                }
                (Ok(Written { len: 1.., offset }), Access::Write) => {
                    ctx.fs.write_latency(fd, *offset)
                }
                _ => ctx.fs.calculate_latency(false),
            };
            Ok((result, latency))
        })?;
        delay(latency).await;
        Ok(result?.len)
    }
}

/// Exposes the simulated file descriptor.
///
/// See [`sync_fs::File`]'s `AsRawFd` impl for details. The fd is the
/// hand-off point for the [`crate::io_uring`] shim.
impl AsRawFd for File {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl AsyncRead for File {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let most = buf.remaining().min(self.max_buf);
        let read = self.inner.read(buf.initialize_unfilled_to(most));
        Poll::Ready(read.map(|n| buf.advance(n)))
    }
}

impl AsyncWrite for File {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize>> {
        let most = buf.len().min(self.max_buf);
        Poll::Ready(self.inner.write(&buf[..most]))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(self.inner.flush())
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for File {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> Result<()> {
        self.inner.seek(position)?;
        Ok(())
    }

    fn poll_complete(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<u64>> {
        Poll::Ready(self.inner.stream_position())
    }
}

/// Options for opening files asynchronously.
///
/// This is the async equivalent of [`std::fs::OpenOptions`](crate::shim::std::fs::OpenOptions).
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    inner: sync_fs::OpenOptions,
}

impl From<sync_fs::OpenOptions> for OpenOptions {
    fn from(inner: sync_fs::OpenOptions) -> Self {
        Self { inner }
    }
}

impl OpenOptions {
    /// Creates a blank new set of options.
    pub fn new() -> Self {
        Self {
            inner: sync_fs::OpenOptions::new(),
        }
    }

    /// Sets the option for read access.
    pub fn read(&mut self, read: bool) -> &mut Self {
        self.inner.read(read);
        self
    }

    /// Sets the option for write access.
    pub fn write(&mut self, write: bool) -> &mut Self {
        self.inner.write(write);
        self
    }

    /// Sets the option for append mode.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.inner.append(append);
        self
    }

    /// Sets the option for truncating a file.
    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.inner.truncate(truncate);
        self
    }

    /// Sets the option for creating a new file.
    pub fn create(&mut self, create: bool) -> &mut Self {
        self.inner.create(create);
        self
    }

    /// Sets the option for creating a new file, failing if it already exists.
    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.inner.create_new(create_new);
        self
    }

    /// Sets the option for direct I/O (bypasses page cache).
    ///
    /// This is a cross-platform way to bypass the page cache. On real systems,
    /// you would use `custom_flags(libc::O_DIRECT)` on Linux or `fcntl(F_NOCACHE)`
    /// on macOS. This method works uniformly in turmoil's simulation.
    ///
    /// When enabled, reads and writes bypass the simulated page cache,
    /// always incurring full I/O latency.
    ///
    /// # Durability Note
    ///
    /// O_DIRECT only affects **latency**, not **durability**. Writes still require
    /// `sync_all()` to become crash-safe. See [`std::fs::OpenOptions::direct_io`]
    /// for details.
    ///
    /// [`std::fs::OpenOptions::direct_io`]: crate::shim::std::fs::OpenOptions::direct_io
    pub fn direct_io(&mut self, direct_io: bool) -> &mut Self {
        self.inner.direct_io(direct_io);
        self
    }

    /// Sets the mode bits that a new file will be created with.
    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.inner.set_mode(mode);
        self
    }

    /// Passes custom flags to the `flags` argument of `open`.
    pub fn custom_flags(&mut self, flags: i32) -> &mut Self {
        self.inner.set_custom_flags(flags);
        self
    }

    /// Opens a file at `path` with the options specified by `self`.
    pub async fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        self.inner.open(path).map(File::from_std)
    }
}

/// A builder for creating directories with options.
#[derive(Debug, Default)]
pub struct DirBuilder {
    inner: sync_fs::DirBuilder,
}

impl DirBuilder {
    /// Creates a new DirBuilder with default options.
    pub fn new() -> Self {
        Self {
            inner: sync_fs::DirBuilder::new(),
        }
    }

    /// Sets the option for recursive directory creation.
    pub fn recursive(&mut self, recursive: bool) -> &mut Self {
        self.inner.recursive(recursive);
        self
    }

    /// Sets the mode to create new directories with.
    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.inner.mode(mode);
        self
    }

    /// Creates the directory at the given path.
    pub async fn create<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.inner.create(path)
    }
}
