//! Async filesystem shims mirroring `tokio::fs`.
//!
//! Provides async wrappers around the [`std::fs`](crate::fs::shim::std::fs) shims.
//!
//! # I/O Latency
//!
//! When I/O latency is configured via [`crate::fs::FsConfig::io_latency`], these
//! async operations will sleep for the configured duration using simulated time.
//! This allows testing code that is sensitive to I/O latency without slowing down
//! tests.
//!
//! Note: Only async operations (this module) have latency. Sync operations via
//! [`crate::fs::shim::std::fs`] complete immediately.
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

use crate::fs::shim::std::fs as sync_fs;
use crate::fs::FsContext;
use std::io::Result;
use std::path::Path;

/// Apply I/O latency if configured (assumes cache miss).
///
/// This uses tokio's simulated time, so it doesn't slow down tests.
async fn apply_io_latency() {
    let latency = FsContext::current(|ctx| ctx.fs.calculate_latency(ctx.rng, false));
    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
}

/// Apply I/O latency for a file read operation, checking page cache.
///
/// - Checks if the page is cached (returns cache hit = fast latency)
/// - Inserts the page into cache after read (if not direct_io)
/// - Direct I/O always bypasses cache
async fn apply_read_latency(path: &Path, offset: u64, direct_io: bool) {
    let latency = FsContext::current(|ctx| {
        let cache_hit = if !direct_io {
            if let Some(cache) = &mut ctx.fs.page_cache {
                let hit = cache.access(path, offset, ctx.rng);
                // Insert into cache after read (even on hit, to update LRU)
                cache.insert(path, offset);
                hit
            } else {
                false
            }
        } else {
            false // O_DIRECT always misses cache
        };

        ctx.fs.calculate_latency(ctx.rng, cache_hit)
    });

    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
}

/// Apply I/O latency for a file write operation, populating page cache.
///
/// - Writes always populate the cache (write-through)
/// - Direct I/O bypasses cache entirely
async fn apply_write_latency(path: &Path, offset: u64, direct_io: bool) {
    let latency = FsContext::current(|ctx| {
        // Writes are always cache misses (data goes to disk)
        // but we populate the cache for subsequent reads
        if !direct_io {
            if let Some(cache) = &mut ctx.fs.page_cache {
                cache.insert(path, offset);
            }
        }

        // Writes always incur full latency (write-through to disk)
        ctx.fs.calculate_latency(ctx.rng, false)
    });

    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
}

// Re-export types that are the same
pub use sync_fs::{DirEntry, FileType, Metadata, Permissions, ReadDir};

/// Async version of [`std::fs::canonicalize`].
pub async fn canonicalize<P: AsRef<Path>>(path: P) -> Result<std::path::PathBuf> {
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
    result
}

/// Async version of [`std::fs::read_link`].
///
/// This operation incurs I/O latency if configured.
pub async fn read_link<P: AsRef<Path>>(path: P) -> Result<std::path::PathBuf> {
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
    sync_fs::symlink(original, link)
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

/// Returns `Ok(true)` if the path exists.
pub async fn try_exists<P: AsRef<Path>>(path: P) -> Result<bool> {
    Ok(sync_fs::metadata(path).is_ok())
}

/// Syncs a directory (turmoil-specific, not in tokio::fs).
///
/// See [`crate::fs::shim::std::fs::sync_dir`] for details.
///
/// This operation incurs I/O latency if configured.
pub async fn sync_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let result = sync_fs::sync_dir(&path);
    apply_io_latency().await;
    result
}

/// An async file handle.
///
/// This is the async equivalent of [`std::fs::File`](crate::fs::shim::std::fs::File).
/// It implements [`tokio::io::AsyncRead`], [`AsyncWrite`](tokio::io::AsyncWrite),
/// and [`AsyncSeek`](tokio::io::AsyncSeek).
#[derive(Debug)]
pub struct File {
    inner: sync_fs::File,
}

impl File {
    /// Opens a file in read-only mode.
    pub async fn open<P: AsRef<Path>>(path: P) -> Result<File> {
        Ok(File {
            inner: sync_fs::File::open(path)?,
        })
    }

    /// Opens a file in write-only mode, creating it if it doesn't exist.
    pub async fn create<P: AsRef<Path>>(path: P) -> Result<File> {
        Ok(File {
            inner: sync_fs::File::create(path)?,
        })
    }

    /// Converts a [`std::fs::File`](crate::fs::shim::std::fs::File) to an async `File`.
    pub fn from_std(std: sync_fs::File) -> File {
        File { inner: std }
    }

    /// Converts this async `File` into a sync [`std::fs::File`](crate::fs::shim::std::fs::File).
    pub fn into_std(self) -> sync_fs::File {
        self.inner
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

    /// Creates a new independently owned handle to the underlying file.
    pub async fn try_clone(&self) -> Result<File> {
        Ok(File {
            inner: self.inner.try_clone()?,
        })
    }

    /// Reads bytes from the file at the specified offset.
    ///
    /// This operation incurs I/O latency if configured. If page cache is enabled,
    /// cache hits result in near-instant reads.
    pub async fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        use std::os::unix::fs::FileExt;
        let result = self.inner.read_at(buf, offset);

        // Apply cache-aware latency
        if let Some(path) = self.inner.path() {
            apply_read_latency(&path, offset, self.inner.is_direct_io()).await;
        } else {
            apply_io_latency().await;
        }

        result
    }

    /// Writes bytes to the file at the specified offset.
    ///
    /// This operation incurs I/O latency if configured. Writes populate
    /// the page cache for subsequent reads.
    pub async fn write_at(&self, buf: &[u8], offset: u64) -> Result<usize> {
        use std::os::unix::fs::FileExt;
        let result = self.inner.write_at(buf, offset);

        // Apply cache-aware latency (writes populate cache)
        if let Some(path) = self.inner.path() {
            apply_write_latency(&path, offset, self.inner.is_direct_io()).await;
        } else {
            apply_io_latency().await;
        }

        result
    }
}

impl tokio::io::AsyncRead for File {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<Result<()>> {
        use std::io::Read;
        let slice = buf.initialize_unfilled();
        match self.inner.read(slice) {
            Ok(n) => {
                buf.advance(n);
                std::task::Poll::Ready(Ok(()))
            }
            Err(e) => std::task::Poll::Ready(Err(e)),
        }
    }
}

impl tokio::io::AsyncWrite for File {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize>> {
        use std::io::Write;
        std::task::Poll::Ready(self.inner.write(buf))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        use std::io::Write;
        std::task::Poll::Ready(self.inner.flush())
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl tokio::io::AsyncSeek for File {
    fn start_seek(mut self: std::pin::Pin<&mut Self>, position: std::io::SeekFrom) -> Result<()> {
        use std::io::Seek;
        self.inner.seek(position)?;
        Ok(())
    }

    fn poll_complete(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<u64>> {
        use std::io::Seek;
        std::task::Poll::Ready(self.inner.stream_position())
    }
}

/// Options for opening files asynchronously.
///
/// This is the async equivalent of [`std::fs::OpenOptions`](crate::fs::shim::std::fs::OpenOptions).
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    inner: sync_fs::OpenOptions,
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
    /// [`std::fs::OpenOptions::direct_io`]: crate::fs::shim::std::fs::OpenOptions::direct_io
    pub fn direct_io(&mut self, direct_io: bool) -> &mut Self {
        self.inner.direct_io(direct_io);
        self
    }

    /// Opens a file at `path` with the options specified by `self`.
    pub async fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        Ok(File {
            inner: self.inner.open(path)?,
        })
    }
}

#[cfg(unix)]
impl std::os::unix::fs::OpenOptionsExt for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.inner.set_mode(mode);
        self
    }

    fn custom_flags(&mut self, flags: i32) -> &mut Self {
        self.inner.set_custom_flags(flags);
        self
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

    /// Creates the directory at the given path.
    pub async fn create<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        self.inner.create(path)
    }
}

#[cfg(unix)]
impl std::os::unix::fs::DirBuilderExt for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::DirBuilderExt::mode(&mut self.inner, mode);
        self
    }
}
