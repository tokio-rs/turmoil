//! Simulated filesystem types mirroring `std::fs`.
//!
//! Provides drop-in replacements for:
//! - [`File`] - Simulated file handle with `std::io::{Read, Write, Seek}` impls
//! - [`OpenOptions`] - Options for opening files
//! - [`Metadata`] - File metadata
//! - [`ReadDir`] - Iterator over directory entries
//! - [`DirEntry`] - An entry inside a directory
//! - [`FileType`] - Representation of a file type
//! - Free functions: [`canonicalize`], [`copy`], [`create_dir`], [`create_dir_all`],
//!   [`metadata`], [`read`], [`read_dir`], [`read_to_string`], [`remove_dir`],
//!   [`remove_dir_all`], [`remove_file`], [`rename`], [`sync_dir`], [`write`]
//!
//! # Durability Model
//!
//! This shim implements POSIX durability semantics. Understanding these semantics
//! is critical for writing crash-safe code.
//!
//! ## File Data vs Directory Entries
//!
//! In POSIX, a file has two distinct pieces that must be persisted:
//!
//! 1. **File data and inode** - The file's contents and metadata (size, timestamps)
//! 2. **Directory entry** - The name→inode mapping in the parent directory
//!
//! These are stored separately and synced separately:
//!
//! - [`File::sync_all`] syncs the file's data and inode
//! - [`sync_dir`] syncs the directory's entries
//!
//! ## Why Both Are Needed
//!
//! After creating a file and calling only `sync_all()`:
//! - The file's data exists on disk
//! - But the directory entry pointing to it may not
//! - On crash, the file is "orphaned" - data exists but is unreachable
//!
//! To make a new file fully durable:
//! ```ignore
//! // Create and write
//! let file = File::create("/data/important.txt")?;
//! file.write_all(b"critical data")?;
//!
//! // Sync file data
//! file.sync_all()?;
//!
//! // Sync directory entry - THIS IS THE PART MOST PEOPLE FORGET
//! sync_dir("/data")?;
//! ```
//!
//! ## Why `sync_dir` Exists
//!
//! In POSIX C, you sync a directory by opening it and calling `fsync()`:
//! ```c
//! int dir_fd = open("/data", O_RDONLY);
//! fsync(dir_fd);
//! ```
//!
//! However, Rust's `std::fs::File::open()` does not support opening directories -
//! it returns an error on most platforms. This means **there is no portable way
//! to sync a directory using `std::fs`**.
//!
//! The [`sync_dir`] function fills this gap for testing. In production code,
//! you would need platform-specific code (e.g., `libc::open` + `libc::fsync`
//! or the `nix` crate) to achieve the same effect.
//!
//! ## Testing Patterns
//!
//! For code that needs to be crash-safe, use conditional compilation:
//!
//! ```ignore
//! #[cfg(test)]
//! use turmoil::fs::shim::std::fs::{File, sync_dir};
//! #[cfg(not(test))]
//! use std::fs::File;
//!
//! fn write_durable(path: &str, data: &[u8]) -> std::io::Result<()> {
//!     let file = File::create(path)?;
//!     file.write_all(data)?;
//!     file.sync_all()?;
//!
//!     // In tests, this calls turmoil's sync_dir
//!     // In production, implement platform-specific directory sync
//!     #[cfg(test)]
//!     if let Some(parent) = std::path::Path::new(path).parent() {
//!         sync_dir(parent)?;
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! # Limitations
//!
//! This simulation has some differences from real filesystems:
//!
//! - **Permissions are informational only**: File modes (e.g., 0o644) are stored
//!   and returned by [`Metadata::permissions`], but they are **not enforced**.
//!   Opening a file with mode 0o000 will still succeed. This simplifies testing
//!   while still allowing code to set and read permission bits.
//!
//! - **All paths must be absolute**: Relative paths are treated as absolute from
//!   root. There is no concept of a current working directory.
//!
//! - **No file locking**: `flock()`/`fcntl()` locking is not implemented.
//!
//! - **Symlink cycle detection**: [`canonicalize`] detects cycles and returns an
//!   error, matching POSIX ELOOP behavior.

use crate::fs::FsContext;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// O_DIRECT flag value for bypassing page cache in the simulation.
///
/// We use Linux's value (0x4000) as the canonical constant. macOS doesn't have
/// O_DIRECT (it uses fcntl F_NOCACHE), but since this is a simulation, we just
/// need a consistent value users can pass to indicate "bypass cache" behavior.
const O_DIRECT: i32 = 0x4000;

/// Creates a new directory at the provided path.
///
/// Like file creation, directory creation is not durable until the parent
/// directory is synced.
pub fn create_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| ctx.fs.mkdir(&path, ctx.now).map_err(Error::other))
}

/// Creates a directory and all of its parent components if they are missing.
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        // Collect all ancestors that need to be created
        let mut to_create = Vec::new();
        let mut current = Some(path.as_path());

        while let Some(p) = current {
            if p.as_os_str().is_empty() {
                break;
            }
            let pb = p.to_path_buf();
            if ctx.fs.dir_exists(&pb) {
                break;
            }
            to_create.push(pb);
            current = p.parent();
        }

        // Create from root down
        for dir in to_create.into_iter().rev() {
            // Skip if it already exists (as file or dir)
            if ctx.fs.dir_exists(&dir) || ctx.fs.file_exists(&dir) {
                continue;
            }
            ctx.fs.mkdir(&dir, ctx.now).map_err(Error::other)?;
        }
        Ok(())
    })
}

/// Removes an empty directory.
pub fn remove_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| ctx.fs.rmdir(&path).map_err(Error::other))
}

/// Syncs a directory, making its entries durable.
///
/// # Why This Function Exists
///
/// In POSIX, you sync a directory by opening it and calling `fsync()`:
/// ```c
/// int dir_fd = open("/data", O_RDONLY);
/// fsync(dir_fd);
/// ```
///
/// However, **Rust's `std::fs::File::open()` does not support opening directories** -
/// it returns an error on most platforms. This means there is no portable way to
/// sync a directory using `std::fs`.
///
/// This function provides the missing capability for turmoil's crash-consistency
/// testing. In production code, you would need platform-specific code (e.g.,
/// `libc::open` + `libc::fsync` or the `nix` crate) to achieve the same effect.
///
/// # What It Does
///
/// Equivalent to calling `fsync()` on a directory file descriptor.
/// It makes the following operations durable:
/// - File and directory creations within this directory
/// - File and directory deletions from this directory
/// - Renames into or out of this directory
/// - The directory's own creation (if it was recently created)
///
/// # POSIX Durability Model
///
/// A file has two distinct pieces that must be persisted separately:
/// 1. **File data and inode** - synced via [`File::sync_all`]
/// 2. **Directory entry** - synced via `sync_dir`
///
/// After `sync_all()` alone, the file's data exists on disk but the directory
/// entry may not. On crash, the file would be "orphaned" - unreachable because
/// no directory entry points to it.
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::shim::std::fs::{File, sync_dir, create_dir};
///
/// // Create directory and make it durable
/// create_dir("/data")?;
/// sync_dir("/")?;  // /data entry is now durable
///
/// // Create file and make it fully durable
/// let file = File::create("/data/important.txt")?;
/// file.write_all_at(b"data", 0)?;
/// file.sync_all()?;      // File data is now durable
/// sync_dir("/data")?;    // File's directory entry is now durable
/// ```
///
/// # Testing Pattern
///
/// Use conditional compilation to call `sync_dir` in tests:
/// ```ignore
/// #[cfg(test)]
/// turmoil::fs::shim::std::fs::sync_dir(parent)?;
///
/// #[cfg(not(test))]
/// platform_specific_dir_sync(parent)?;
/// ```
pub fn sync_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| ctx.fs.sync_dir(&path, ctx.now).map_err(Error::other))
}

/// Removes a file from the filesystem.
///
/// Note: The deletion is not durable until the parent directory is synced.
pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .unlink(&path)
            .map_err(|e| Error::new(ErrorKind::NotFound, e))
    })
}

/// Renames a file or directory.
///
/// This is atomic - if the destination exists, it will be replaced.
/// The rename is not durable until the parent directory is synced.
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    let from = from.as_ref().to_path_buf();
    let to = to.as_ref().to_path_buf();
    FsContext::current(|ctx| ctx.fs.rename(&from, &to).map_err(Error::other))
}

/// Returns `true` if the path points at an existing entity.
///
/// This function follows symbolic links, so if `path` is a symbolic link,
/// it returns whether the target exists.
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::shim::std::fs::{exists, write, create_dir};
///
/// assert!(!exists("/data"));
/// create_dir("/data")?;
/// assert!(exists("/data"));
///
/// write("/data/file.txt", b"hello")?;
/// assert!(exists("/data/file.txt"));
/// ```
pub fn exists<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        // Follow symlinks
        let resolved = if ctx.fs.symlink_exists(&path) {
            match ctx.fs.read_link(&path) {
                Ok(target) => target,
                Err(_) => return false,
            }
        } else {
            path.clone()
        };

        ctx.fs.file_exists(&resolved)
            || ctx.fs.dir_exists(&resolved)
            || ctx.fs.symlink_exists(&resolved)
    })
}

/// Returns metadata for a file or directory.
///
/// This function follows symbolic links. Use [`symlink_metadata`] to get
/// metadata for the symlink itself.
pub fn metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        // Follow symlinks - resolve the target path
        let resolved = if ctx.fs.symlink_exists(&path) {
            ctx.fs
                .read_link(&path)
                .map_err(|e| Error::new(ErrorKind::NotFound, e))?
        } else {
            path.clone()
        };

        // Check if it's a file
        if ctx.fs.file_exists(&resolved) {
            let len = ctx.fs.file_len(&resolved);
            let (crtime, mtime, ctime) = ctx.fs.file_timestamps(&resolved).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            let mode = ctx.fs.file_mode(&resolved).unwrap_or(0o644);
            let nlink = ctx.fs.file_nlink(&resolved);

            return Ok(Metadata {
                len,
                is_dir: false,
                is_symlink: false,
                crtime,
                mtime,
                ctime,
                mode,
                nlink,
            });
        }

        // Check if it's a directory
        if ctx.fs.dir_exists(&resolved) {
            let (crtime, mtime, ctime) = ctx.fs.dir_timestamps(&resolved).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            let mode = ctx.fs.dir_mode(&resolved).unwrap_or(0o755);

            return Ok(Metadata {
                len: 0,
                is_dir: true,
                is_symlink: false,
                crtime,
                mtime,
                ctime,
                mode,
                nlink: 2, // Directories have at least 2 links (. and parent's entry)
            });
        }

        Err(Error::new(
            ErrorKind::NotFound,
            "file or directory not found",
        ))
    })
}

/// Returns an iterator over the entries within a directory.
///
/// The iterator yields instances of `Result<DirEntry>`. Each entry can be
/// inspected for its path, file name, and metadata.
pub fn read_dir<P: AsRef<Path>>(path: P) -> Result<ReadDir> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        if !ctx.fs.dir_exists(&path) {
            return Err(Error::new(ErrorKind::NotFound, "directory not found"));
        }

        let entries = ctx.fs.dir_entries(&path);
        Ok(ReadDir {
            entries: entries.into_iter(),
        })
    })
}

/// Read the entire contents of a file into a bytes vector.
///
/// This is a convenience function for using [`File::open`] and reading the
/// entire file contents.
pub fn read<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len() as usize;
    let mut buf = vec![0u8; len];
    if len > 0 {
        file.read_at_internal(&mut buf, 0)?;
    }
    Ok(buf)
}

/// Read the entire contents of a file into a string.
///
/// This is a convenience function for using [`File::open`] and reading the
/// entire file contents as a UTF-8 string.
pub fn read_to_string<P: AsRef<Path>>(path: P) -> Result<String> {
    let bytes = read(path)?;
    String::from_utf8(bytes).map_err(|e| Error::new(ErrorKind::InvalidData, e))
}

/// Write a slice as the entire contents of a file.
///
/// This function will create a file if it does not exist, and will entirely
/// replace its contents if it does.
pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> Result<()> {
    let file = File::create(path)?;
    let contents = contents.as_ref();
    if !contents.is_empty() {
        file.write_at_internal(contents, 0)?;
    }
    Ok(())
}

/// Copies the contents of one file to another.
///
/// This function will overwrite the contents of `to`. Returns the number of
/// bytes copied.
///
/// Note: This does not copy file permissions or other metadata.
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<u64> {
    let contents = read(from)?;
    let len = contents.len() as u64;
    write(to, contents)?;
    Ok(len)
}

/// Iterator over directory entries.
///
/// Returned by [`read_dir`].
pub struct ReadDir {
    entries: std::vec::IntoIter<std::path::PathBuf>,
}

impl Iterator for ReadDir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.entries.next().map(|path| Ok(DirEntry { path }))
    }
}

/// An entry inside a directory.
///
/// Returned by iterating over [`ReadDir`].
pub struct DirEntry {
    path: std::path::PathBuf,
}

impl DirEntry {
    /// Returns the full path to this entry.
    pub fn path(&self) -> std::path::PathBuf {
        self.path.clone()
    }

    /// Returns the file name of this entry.
    pub fn file_name(&self) -> std::ffi::OsString {
        self.path
            .file_name()
            .map(|s| s.to_owned())
            .unwrap_or_default()
    }

    /// Returns the metadata for this entry.
    ///
    /// This does not follow symlinks - if the entry is a symlink, it returns
    /// metadata for the symlink itself, not the target.
    pub fn metadata(&self) -> Result<Metadata> {
        symlink_metadata(&self.path)
    }

    /// Returns the file type for this entry.
    pub fn file_type(&self) -> Result<FileType> {
        // Use symlink_metadata to not follow symlinks
        let meta = symlink_metadata(&self.path)?;
        Ok(FileType {
            is_dir: meta.is_dir(),
            is_symlink: meta.is_symlink(),
        })
    }
}

/// Representation of a file type.
///
/// Returned by [`DirEntry::file_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType {
    is_dir: bool,
    is_symlink: bool,
}

impl FileType {
    /// Returns true if this file type is a directory.
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Returns true if this file type is a regular file.
    pub fn is_file(&self) -> bool {
        !self.is_dir && !self.is_symlink
    }

    /// Returns true if this file type is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}

/// A simulated file handle.
///
/// Drop-in replacement for `std::fs::File` that stores data in turmoil's
/// per-host simulated filesystem.
#[derive(Debug)]
pub struct File {
    /// File descriptor used to identify this file in the host's Fs
    fd: u64,
    /// Whether the file is readable
    readable: bool,
    /// Whether the file is writable
    writable: bool,
    /// Whether the file is in append mode
    append_mode: bool,
    /// Whether this file bypasses the page cache (O_DIRECT)
    direct_io: bool,
    /// Current cursor position for read()/write()
    cursor: std::sync::Mutex<u64>,
}

impl File {
    /// Attempts to open a file in read-only mode.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<File> {
        OpenOptions::new().read(true).open(path)
    }

    /// Opens a file in write-only mode, creating it if it doesn't exist,
    /// and truncating it if it does.
    pub fn create<P: AsRef<Path>>(path: P) -> Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }

    /// Queries metadata about the file.
    pub fn metadata(&self) -> Result<Metadata> {
        FsContext::current(|ctx| {
            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?;

            let len = ctx.fs.file_len(path);
            let (crtime, mtime, ctime) = ctx.fs.file_timestamps(path).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            let mode = ctx.fs.file_mode(path).unwrap_or(0o644);
            let nlink = ctx.fs.file_nlink(path);

            Ok(Metadata {
                len,
                is_dir: false,     // File handles are always for regular files
                is_symlink: false, // File handles are never symlinks
                crtime,
                mtime,
                ctime,
                mode,
                nlink,
            })
        })
    }

    /// Truncates or extends the file to the specified size.
    pub fn set_len(&self, size: u64) -> Result<()> {
        if !self.writable {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "file not opened for writing",
            ));
        }

        FsContext::current(|mut ctx| {
            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();

            ctx.fs.set_file_len(&path, size, ctx.now);
            let sync_prob = ctx.fs.sync_probability;

            // Maybe randomly sync file data (simulates OS/disk flushing pending operations)
            // Note: This only syncs file data, not directory entries. Real OSes may flush
            // file data to disk but won't necessarily sync directory entries.
            if sync_prob > 0.0 && ctx.random_bool(sync_prob) {
                let _ = ctx.fs.sync_file(&path);
            }

            Ok(())
        })
    }

    /// Attempts to sync all OS-internal metadata and data to disk.
    ///
    /// This is equivalent to calling `fsync()` on the file descriptor. It makes
    /// the file's **data and inode** durable, including:
    /// - File contents (all writes)
    /// - File size (truncate/extend operations)
    /// - File timestamps (mtime, ctime)
    ///
    /// # Important: Directory Entry Not Synced
    ///
    /// **This does NOT sync the directory entry.** In POSIX, a file's data/inode
    /// and its directory entry are separate. After `sync_all()`:
    /// - The file's data is durable on disk
    /// - But the directory entry pointing to it may not be
    /// - On crash, the file could be "orphaned" (data exists but unreachable)
    ///
    /// To make a newly created file fully durable, you must also call
    /// [`sync_dir`] on the parent directory.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use turmoil::fs::shim::std::fs::{File, sync_dir};
    ///
    /// let file = File::create("/data/file.txt")?;
    /// file.write_all(b"important")?;
    ///
    /// // Sync file data - necessary but not sufficient for new files
    /// file.sync_all()?;
    ///
    /// // Sync directory entry - required for new files to survive crash
    /// sync_dir("/data")?;
    /// ```
    ///
    /// # When Directory Sync Is Not Needed
    ///
    /// If overwriting an **existing** file that was previously synced, you only
    /// need `sync_all()` - the directory entry already exists and is durable.
    pub fn sync_all(&self) -> Result<()> {
        FsContext::current(|ctx| {
            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();

            ctx.fs.sync_file(&path).map_err(Error::other)
        })
    }

    /// Attempts to sync file data to disk, but not all metadata.
    ///
    /// This is equivalent to calling `fdatasync()` on the file descriptor.
    /// It syncs file contents and size, but may not sync all metadata.
    ///
    /// In turmoil's implementation, this behaves identically to [`sync_all`]
    /// for file data. The distinction matters more for the directory entry:
    /// neither `sync_data` nor `sync_all` syncs the directory entry - you
    /// need [`sync_dir`] for that.
    pub fn sync_data(&self) -> Result<()> {
        FsContext::current(|ctx| {
            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();

            ctx.fs.sync_file_data(&path).map_err(Error::other)
        })
    }

    /// Creates a new independently owned handle to the underlying file.
    ///
    /// The returned `File` is a reference to the same state that this object
    /// references. Both handles will read and write at independent cursors.
    pub fn try_clone(&self) -> Result<File> {
        FsContext::current(|ctx| {
            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();

            // Allocate new fd and register it
            let new_fd = ctx.fs.alloc_fd();
            ctx.fs.open_handles.insert(new_fd, path);

            Ok(File {
                fd: new_fd,
                readable: self.readable,
                writable: self.writable,
                append_mode: self.append_mode,
                direct_io: self.direct_io,
                cursor: std::sync::Mutex::new(0),
            })
        })
    }

    /// Returns whether this file was opened with O_DIRECT (bypasses page cache).
    pub fn is_direct_io(&self) -> bool {
        self.direct_io
    }

    /// Returns the path this file was opened with.
    ///
    /// Returns `None` if the file handle is no longer valid.
    pub fn path(&self) -> Option<PathBuf> {
        FsContext::current(|ctx| ctx.fs.open_handles.get(&self.fd).cloned())
    }

    /// Internal read implementation.
    pub(crate) fn read_at_internal(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        if !self.readable {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "file not opened for reading",
            ));
        }

        FsContext::current(|mut ctx| {
            // Check for random I/O error
            let io_err_prob = ctx.fs.io_error_probability;
            if io_err_prob > 0.0 && ctx.random_bool(io_err_prob) {
                return Err(Error::other("I/O error"));
            }

            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();
            let corruption_prob = ctx.fs.corruption_probability;

            let n = ctx.fs.read_file(&path, buf, offset);

            // Check for random silent corruption
            if n > 0 && corruption_prob > 0.0 && ctx.random_bool(corruption_prob) {
                // Corrupt a random byte in the buffer by flipping bits
                let corrupt_offset = ctx.random_range(0..n);
                let corrupt_byte = ctx.random_u8();
                buf[corrupt_offset] ^= corrupt_byte.max(1); // Ensure at least one bit flip

                // Fire barrier trigger for observation (if barriers feature enabled)
                #[cfg(feature = "unstable-barriers")]
                crate::barriers::trigger_noop(crate::fs::FsCorruption {
                    path: path.clone(),
                    offset: offset + corrupt_offset as u64,
                    len: 1,
                });
            }

            Ok(n)
        })
    }

    /// Internal write implementation.
    pub(crate) fn write_at_internal(&self, buf: &[u8], offset: u64) -> Result<usize> {
        if !self.writable {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "file not opened for writing",
            ));
        }

        FsContext::current(|mut ctx| {
            // Check for random I/O error
            let io_err_prob = ctx.fs.io_error_probability;
            if io_err_prob > 0.0 && ctx.random_bool(io_err_prob) {
                return Err(Error::other("I/O error"));
            }

            let path = ctx
                .fs
                .open_handles
                .get(&self.fd)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "file handle not found"))?
                .clone();

            // Check capacity
            let current_len = ctx.fs.file_len(&path);
            let write_end = offset + buf.len() as u64;
            let additional = write_end.saturating_sub(current_len);
            if additional > 0 {
                ctx.fs.check_space(additional).map_err(Error::other)?;
            }

            ctx.fs.write_file(&path, offset, buf, ctx.now);
            let sync_prob = ctx.fs.sync_probability;

            // Maybe randomly sync file data (simulates OS/disk flushing pending operations)
            // Note: This only syncs file data, not directory entries. Real OSes may flush
            // file data to disk but won't necessarily sync directory entries.
            if sync_prob > 0.0 && ctx.random_bool(sync_prob) {
                let _ = ctx.fs.sync_file(&path);
            }

            Ok(buf.len())
        })
    }

    /// Create a File from internal state (used by OpenOptions).
    pub(crate) fn from_parts(
        fd: u64,
        readable: bool,
        writable: bool,
        append_mode: bool,
        direct_io: bool,
    ) -> Self {
        Self {
            fd,
            readable,
            writable,
            append_mode,
            direct_io,
            cursor: std::sync::Mutex::new(0),
        }
    }
}

impl std::io::Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut cursor = self.cursor.lock().unwrap();
        let n = self.read_at_internal(buf, *cursor)?;
        *cursor += n as u64;
        Ok(n)
    }
}

impl std::io::Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let mut cursor = self.cursor.lock().unwrap();
        let offset = if self.append_mode {
            // Append mode: always write at end
            FsContext::current(|ctx| {
                let path = ctx.fs.open_handles.get(&self.fd)?;
                Some(ctx.fs.file_len(path))
            })
            .unwrap_or(*cursor)
        } else {
            *cursor
        };
        let n = self.write_at_internal(buf, offset)?;
        *cursor = offset + n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl std::io::Seek for File {
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64> {
        let mut cursor = self.cursor.lock().unwrap();
        let file_len = FsContext::current(|ctx| {
            let path = ctx.fs.open_handles.get(&self.fd)?;
            Some(ctx.fs.file_len(path))
        });

        let new_pos = match pos {
            std::io::SeekFrom::Start(offset) => offset as i64,
            std::io::SeekFrom::End(offset) => {
                file_len.ok_or_else(|| Error::new(ErrorKind::NotFound, "file not found"))? as i64
                    + offset
            }
            std::io::SeekFrom::Current(offset) => *cursor as i64 + offset,
        };

        if new_pos < 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid seek to a negative position",
            ));
        }

        *cursor = new_pos as u64;
        Ok(*cursor)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        FsContext::current_if_set(|ctx| {
            ctx.fs.open_handles.swap_remove(&self.fd);
        });
    }
}

/// Metadata information about a file.
#[derive(Debug, Clone)]
pub struct Metadata {
    len: u64,
    is_dir: bool,
    is_symlink: bool,
    /// Creation time (birthtime)
    crtime: Duration,
    /// Modification time
    mtime: Duration,
    /// Change time (inode change).
    /// Exposed via MetadataExt::ctime().
    ctime: Duration,
    /// Unix permission mode bits
    mode: u32,
    /// Number of hard links to this file
    nlink: u64,
}

impl Metadata {
    /// Returns the size of the file in bytes.
    #[allow(clippy::len_without_is_empty)] // is_empty() is not part of std::fs::Metadata
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Returns true if this metadata is for a regular file.
    pub fn is_file(&self) -> bool {
        !self.is_dir && !self.is_symlink
    }

    /// Returns true if this metadata is for a directory.
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Returns true if this metadata is for a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.is_symlink
    }

    /// Returns the file type for this metadata.
    pub fn file_type(&self) -> FileType {
        FileType {
            is_dir: self.is_dir,
            is_symlink: self.is_symlink,
        }
    }

    /// Returns the last modification time.
    ///
    /// This is updated when file contents are modified (writes, truncate).
    pub fn modified(&self) -> Result<SystemTime> {
        Ok(UNIX_EPOCH + self.mtime)
    }

    /// Returns the last access time.
    ///
    /// Note: turmoil simulates noatime behavior, so this always returns
    /// the creation time (atime is not updated on reads).
    pub fn accessed(&self) -> Result<SystemTime> {
        // noatime: return creation time instead of tracking access time
        Ok(UNIX_EPOCH + self.crtime)
    }

    /// Returns the creation time (birthtime).
    ///
    /// Note: Not all platforms support creation time. On platforms that don't
    /// support it, `std::fs::Metadata::created()` returns an error. Our
    /// simulation always tracks it.
    pub fn created(&self) -> Result<SystemTime> {
        Ok(UNIX_EPOCH + self.crtime)
    }

    /// Returns the permissions of the file.
    pub fn permissions(&self) -> Permissions {
        Permissions { mode: self.mode }
    }
}

impl std::os::unix::fs::MetadataExt for Metadata {
    fn dev(&self) -> u64 {
        0 // Simulated device
    }

    fn ino(&self) -> u64 {
        0 // Simulated inode
    }

    fn mode(&self) -> u32 {
        // Include file type bits: regular file (0o100000), directory (0o40000), symlink (0o120000)
        let type_bits = if self.is_symlink {
            0o120000
        } else if self.is_dir {
            0o040000
        } else {
            0o100000
        };
        type_bits | self.mode
    }

    fn nlink(&self) -> u64 {
        self.nlink
    }

    fn uid(&self) -> u32 {
        0 // Simulated uid
    }

    fn gid(&self) -> u32 {
        0 // Simulated gid
    }

    fn rdev(&self) -> u64 {
        0 // Not a device file
    }

    fn size(&self) -> u64 {
        self.len
    }

    fn atime(&self) -> i64 {
        // noatime: return creation time
        self.crtime.as_secs() as i64
    }

    fn atime_nsec(&self) -> i64 {
        self.crtime.subsec_nanos() as i64
    }

    fn mtime(&self) -> i64 {
        self.mtime.as_secs() as i64
    }

    fn mtime_nsec(&self) -> i64 {
        self.mtime.subsec_nanos() as i64
    }

    fn ctime(&self) -> i64 {
        self.ctime.as_secs() as i64
    }

    fn ctime_nsec(&self) -> i64 {
        self.ctime.subsec_nanos() as i64
    }

    fn blksize(&self) -> u64 {
        4096 // Simulated block size
    }

    fn blocks(&self) -> u64 {
        // Number of 512-byte blocks allocated
        self.len.div_ceil(512)
    }
}

/// Options and flags for opening files.
///
/// Drop-in replacement for `std::fs::OpenOptions`.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    /// Unix mode bits for file creation (default: 0o644)
    mode: u32,
    /// Custom flags (for OpenOptionsExt compatibility)
    custom_flags: i32,
    /// Direct I/O mode - bypasses page cache (turmoil-specific)
    direct_io: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            mode: 0o644,
            custom_flags: 0,
            direct_io: false,
        }
    }
}

impl OpenOptions {
    /// Creates a blank new set of options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the option for read access.
    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    /// Sets the option for write access.
    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    /// Sets the option for append mode.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Sets the option for truncating a file.
    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    /// Sets the option for creating a new file.
    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    /// Sets the option for creating a new file, failing if it already exists.
    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    /// Sets the option for direct I/O (bypasses page cache).
    ///
    /// This is a turmoil-specific extension that works on all platforms.
    /// On real systems, you would use `custom_flags(libc::O_DIRECT)` on Linux
    /// or `fcntl(F_NOCACHE)` on macOS.
    ///
    /// When enabled, reads and writes bypass the simulated page cache,
    /// always incurring full I/O latency.
    ///
    /// # Durability Note
    ///
    /// In this simulation, O_DIRECT only affects **latency**, not **durability**.
    /// Writes still require `sync_all()` to become crash-safe, regardless of
    /// whether O_DIRECT is used. This is actually realistic: on real systems,
    /// O_DIRECT bypasses the OS page cache but data can still sit in the drive's
    /// volatile write cache. Only `fsync()` guarantees data reaches persistent
    /// media.
    ///
    /// In practice, O_DIRECT writes are "closer" to disk (smaller device buffer
    /// vs large OS page cache), but this simulation takes the conservative
    /// approach of treating all non-synced writes equally for crash testing.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use turmoil::fs::shim::std::fs::OpenOptions;
    ///
    /// let file = OpenOptions::new()
    ///     .read(true)
    ///     .direct_io(true)
    ///     .open("/data/file.txt")?;
    /// ```
    pub fn direct_io(&mut self, direct_io: bool) -> &mut Self {
        self.direct_io = direct_io;
        self
    }

    /// Sets the mode bits for file creation.
    ///
    /// This is used internally by the OpenOptionsExt trait implementation.
    pub(crate) fn set_mode(&mut self, mode: u32) {
        self.mode = mode;
    }

    /// Sets custom flags for file opening.
    ///
    /// This is used internally by the OpenOptionsExt trait implementation.
    /// Note: custom_flags are currently stored but not used in the simulation.
    pub(crate) fn set_custom_flags(&mut self, flags: i32) {
        self.custom_flags = flags;
    }

    /// Opens a file at `path` with the options specified by `self`.
    pub fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = path.as_ref().to_path_buf();

        FsContext::current(|ctx| {
            // Follow symlinks to resolve the actual file path
            let resolved_path = if ctx.fs.symlink_exists(&path) {
                ctx.fs
                    .read_link(&path)
                    .map_err(|e| Error::new(ErrorKind::NotFound, e))?
            } else {
                path.clone()
            };

            let file_exists = ctx.fs.file_exists(&resolved_path);

            // Handle create_new: fail if file exists
            if self.create_new && file_exists {
                return Err(Error::new(ErrorKind::AlreadyExists, "file already exists"));
            }

            // Handle missing file
            if !file_exists {
                if self.create || self.create_new {
                    // Check parent directory exists
                    if !ctx.fs.parent_exists(&resolved_path) {
                        return Err(Error::new(
                            ErrorKind::NotFound,
                            "parent directory not found",
                        ));
                    }
                    // Create new file with mode
                    ctx.fs
                        .create_file_with_mode(&resolved_path, ctx.now, self.mode);
                } else {
                    return Err(Error::new(ErrorKind::NotFound, "file not found"));
                }
            }

            // Handle truncate (pending until sync, like other operations)
            if self.truncate && self.write {
                ctx.fs.set_file_len(&resolved_path, 0, ctx.now);
            }

            // Allocate file descriptor - store the resolved path
            let fd = ctx.fs.alloc_fd();
            ctx.fs.open_handles.insert(fd, resolved_path);

            // Check for direct I/O (bypasses page cache)
            // Can be set via direct_io() method or custom_flags(O_DIRECT)
            let direct_io = self.direct_io || (self.custom_flags & O_DIRECT) != 0;

            Ok(File::from_parts(
                fd,
                self.read,
                self.write || self.append,
                self.append,
                direct_io,
            ))
        })
    }
}

/// Representation of file permissions.
///
/// Drop-in replacement for `std::fs::Permissions`.
///
/// # Note
///
/// Permissions are **informational only** in turmoil's simulation. The mode bits
/// are stored and can be queried, but they are not enforced during file operations.
/// For example, a file with mode 0o000 can still be opened for reading and writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    mode: u32,
}

impl Permissions {
    /// Returns true if these permissions describe a readonly (unwritable) file.
    pub fn readonly(&self) -> bool {
        // Check if write bits are not set for owner
        (self.mode & 0o200) == 0
    }

    /// Modifies the readonly flag for this set of permissions.
    pub fn set_readonly(&mut self, readonly: bool) {
        if readonly {
            // Clear write bits for owner, group, others
            self.mode &= !0o222;
        } else {
            // Set write bit for owner
            self.mode |= 0o200;
        }
    }

    /// Create permissions from mode bits.
    pub fn from_mode(mode: u32) -> Self {
        Self { mode }
    }

    /// Returns the underlying raw mode bits.
    pub fn mode(&self) -> u32 {
        self.mode
    }
}

impl std::os::unix::fs::PermissionsExt for Permissions {
    fn mode(&self) -> u32 {
        self.mode
    }

    fn set_mode(&mut self, mode: u32) {
        self.mode = mode;
    }

    fn from_mode(mode: u32) -> Self {
        Permissions { mode }
    }
}

/// Set the permissions of a file or directory.
pub fn set_permissions<P: AsRef<Path>>(path: P, perm: Permissions) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .set_permissions(&path, perm.mode, ctx.now)
            .map_err(Error::other)
    })
}

/// Creates a new symbolic link on the filesystem.
///
/// The `link` path will be a symbolic link pointing to the `original` path.
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref().to_path_buf();
    let link = link.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .create_symlink(&link, &original, ctx.now)
            .map_err(Error::other)
    })
}

/// Reads the target of a symbolic link.
pub fn read_link<P: AsRef<Path>>(path: P) -> Result<std::path::PathBuf> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .read_link(&path)
            .map_err(|e| Error::new(ErrorKind::NotFound, e))
    })
}

/// Returns metadata for a path without following symlinks.
///
/// If `path` is a symlink, this returns metadata for the symlink itself,
/// not the target.
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        // Check if it's a symlink first (don't follow)
        if ctx.fs.symlink_exists(&path) {
            let (crtime, mtime, ctime) = ctx.fs.symlink_timestamps(&path).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            // Symlinks have mode 0o777 (lrwxrwxrwx) on most Unix systems
            return Ok(Metadata {
                len: 0,
                is_dir: false,
                is_symlink: true,
                crtime,
                mtime,
                ctime,
                mode: 0o777,
                nlink: 1, // Symlinks always have nlink=1
            });
        }

        // Otherwise, same as metadata()
        if ctx.fs.file_exists(&path) {
            let len = ctx.fs.file_len(&path);
            let (crtime, mtime, ctime) = ctx.fs.file_timestamps(&path).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            let mode = ctx.fs.file_mode(&path).unwrap_or(0o644);
            let nlink = ctx.fs.file_nlink(&path);

            return Ok(Metadata {
                len,
                is_dir: false,
                is_symlink: false,
                crtime,
                mtime,
                ctime,
                mode,
                nlink,
            });
        }

        if ctx.fs.dir_exists(&path) {
            let (crtime, mtime, ctime) = ctx.fs.dir_timestamps(&path).unwrap_or((
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ));
            let mode = ctx.fs.dir_mode(&path).unwrap_or(0o755);

            return Ok(Metadata {
                len: 0,
                is_dir: true,
                is_symlink: false,
                crtime,
                mtime,
                ctime,
                mode,
                nlink: 2, // Directories have at least 2 links
            });
        }

        Err(Error::new(
            ErrorKind::NotFound,
            "file or directory not found",
        ))
    })
}

/// Creates a hard link on the filesystem.
///
/// The `link` path will be a hard link to the `original` file.
/// Both paths will refer to the same file content.
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let original = original.as_ref().to_path_buf();
    let link = link.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .create_hard_link(&link, &original, ctx.now)
            .map_err(Error::other)
    })
}

/// A builder for creating directories with options.
///
/// Drop-in replacement for `std::fs::DirBuilder`.
#[derive(Debug, Default)]
pub struct DirBuilder {
    recursive: bool,
    mode: u32,
}

impl DirBuilder {
    /// Creates a new DirBuilder with default options.
    pub fn new() -> Self {
        Self {
            recursive: false,
            mode: 0o755,
        }
    }

    /// Sets the option for recursive directory creation.
    pub fn recursive(&mut self, recursive: bool) -> &mut Self {
        self.recursive = recursive;
        self
    }

    /// Creates the directory at the given path.
    pub fn create<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        if self.recursive {
            create_dir_all_with_mode(path, self.mode)
        } else {
            create_dir_with_mode(path, self.mode)
        }
    }
}

impl std::os::unix::fs::DirBuilderExt for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }
}

/// Create a directory with specified mode.
fn create_dir_with_mode<P: AsRef<Path>>(path: P, mode: u32) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        ctx.fs
            .mkdir_with_mode(&path, ctx.now, mode)
            .map_err(Error::other)
    })
}

/// Create directories recursively with specified mode.
fn create_dir_all_with_mode<P: AsRef<Path>>(path: P, mode: u32) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        // Collect all ancestors that need to be created
        let mut to_create = Vec::new();
        let mut current = Some(path.as_path());

        while let Some(p) = current {
            if p.as_os_str().is_empty() {
                break;
            }
            let pb = p.to_path_buf();
            if ctx.fs.dir_exists(&pb) {
                break;
            }
            to_create.push(pb);
            current = p.parent();
        }

        // Create from root down
        for dir in to_create.into_iter().rev() {
            if ctx.fs.dir_exists(&dir) || ctx.fs.file_exists(&dir) {
                continue;
            }
            ctx.fs
                .mkdir_with_mode(&dir, ctx.now, mode)
                .map_err(Error::other)?;
        }
        Ok(())
    })
}

/// Returns the canonical, absolute form of a path with all intermediate
/// components normalized and symbolic links resolved.
///
/// # Errors
///
/// Returns an error if the path does not exist or if any component of the
/// path is not a directory (except the final component).
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::shim::std::fs::{canonicalize, create_dir, symlink, write};
///
/// create_dir("/data")?;
/// write("/data/file.txt", b"hello")?;
/// symlink("/data/file.txt", "/link")?;
///
/// // Resolves symlink to actual path
/// assert_eq!(canonicalize("/link")?, PathBuf::from("/data/file.txt"));
///
/// // Normalizes . and ..
/// assert_eq!(canonicalize("/data/../data/./file.txt")?, PathBuf::from("/data/file.txt"));
/// ```
pub fn canonicalize<P: AsRef<Path>>(path: P) -> Result<std::path::PathBuf> {
    use std::collections::HashSet;

    /// Maximum number of symlinks to follow before assuming a cycle.
    /// POSIX typically allows 8-40 symlinks (Linux uses 40, macOS uses 32).
    const MAX_SYMLINK_FOLLOWS: usize = 40;

    let path = path.as_ref();
    FsContext::current(|ctx| {
        // Start with an absolute path
        let mut result = PathBuf::new();

        // Track symlinks we've followed to detect cycles
        let mut visited_symlinks: HashSet<PathBuf> = HashSet::new();
        let mut symlink_count = 0;

        // Handle absolute vs relative paths
        // In our simulation, all paths should be absolute (start with /)
        if path.is_absolute() {
            result.push("/");
        } else {
            // For relative paths, we'd need a current working directory
            // For simplicity, treat as absolute from root
            result.push("/");
        }

        // Process each component
        for component in path.components() {
            match component {
                std::path::Component::RootDir => {
                    // Already handled above
                }
                std::path::Component::CurDir => {
                    // . - skip it
                }
                std::path::Component::ParentDir => {
                    // .. - go up one level
                    result.pop();
                }
                std::path::Component::Normal(name) => {
                    result.push(name);

                    // Check if this component is a symlink and resolve it
                    // Keep resolving while the current path is a symlink
                    while ctx.fs.symlink_exists(&result) {
                        // Check for too many symlinks (ELOOP)
                        symlink_count += 1;
                        if symlink_count > MAX_SYMLINK_FOLLOWS {
                            return Err(Error::other("too many levels of symbolic links"));
                        }

                        // Check for cycle: have we visited this exact symlink before?
                        if !visited_symlinks.insert(result.clone()) {
                            return Err(Error::other("symbolic link loop detected"));
                        }

                        let target = ctx
                            .fs
                            .read_link(&result)
                            .map_err(|e| Error::new(ErrorKind::NotFound, e))?;

                        // If target is absolute, replace result; otherwise append
                        if target.is_absolute() {
                            result = target;
                        } else {
                            result.pop(); // Remove the symlink name
                            result.push(target);
                        }
                    }
                }
                std::path::Component::Prefix(_) => {
                    // Windows-only, ignore in our Unix simulation
                }
            }
        }

        // Verify the final path exists
        if !ctx.fs.file_exists(&result)
            && !ctx.fs.dir_exists(&result)
            && !ctx.fs.symlink_exists(&result)
        {
            return Err(Error::new(ErrorKind::NotFound, "path does not exist"));
        }

        Ok(result)
    })
}

/// Removes a directory and all of its contents.
///
/// This function recursively removes all files and subdirectories within
/// the specified directory, then removes the directory itself.
///
/// # Errors
///
/// Returns an error if:
/// - The path does not exist
/// - The path is not a directory
/// - Any file or subdirectory cannot be removed
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::shim::std::fs::{create_dir_all, write, remove_dir_all, metadata};
///
/// // Create nested structure
/// create_dir_all("/data/subdir")?;
/// write("/data/file1.txt", b"hello")?;
/// write("/data/subdir/file2.txt", b"world")?;
///
/// // Remove everything
/// remove_dir_all("/data")?;
///
/// // Directory no longer exists
/// assert!(metadata("/data").is_err());
/// ```
pub fn remove_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = path.as_ref().to_path_buf();
    FsContext::current(|ctx| {
        if !ctx.fs.dir_exists(&path) {
            return Err(Error::new(ErrorKind::NotFound, "directory not found"));
        }

        // Collect all entries to remove (we need to collect first to avoid
        // modifying while iterating)
        remove_dir_contents_recursive(ctx.fs, &path)?;

        // Finally remove the directory itself
        ctx.fs.rmdir(&path).map_err(Error::other)
    })
}

/// Helper function to recursively remove directory contents.
fn remove_dir_contents_recursive(fs: &mut crate::fs::Fs, path: &Path) -> Result<()> {
    // Get all entries in this directory
    let entries = fs.dir_entries(path);

    for entry_path in entries {
        if fs.dir_exists(&entry_path) {
            // Recursively remove subdirectory contents
            remove_dir_contents_recursive(fs, &entry_path)?;
            // Remove the now-empty subdirectory
            fs.rmdir(&entry_path).map_err(Error::other)?;
        } else if fs.file_exists(&entry_path) || fs.symlink_exists(&entry_path) {
            // Remove file or symlink
            fs.unlink(&entry_path).map_err(Error::other)?;
        }
    }

    Ok(())
}
