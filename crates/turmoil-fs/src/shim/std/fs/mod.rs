//! Simulated filesystem types mirroring `std::fs`.
//!
//! Provides drop-in replacements for:
//! - [`File`] - Simulated file handle with `std::io::{Read, Write, Seek}` impls
//! - [`OpenOptions`] - Options for opening files
//! - [`DirBuilder`] - Options for creating directories
//! - [`Metadata`] - File metadata
//! - [`Permissions`] - The permission bits of a file
//! - [`FileTimes`] - Timestamps to set on a file
//! - [`ReadDir`] - Iterator over directory entries
//! - [`DirEntry`] - An entry inside a directory
//! - [`FileType`] - Representation of a file type
//! - Free functions: [`canonicalize`], [`copy`], [`create_dir`], [`create_dir_all`],
//!   [`exists`], [`hard_link`], [`metadata`], [`read`], [`read_dir`], [`read_link`],
//!   [`read_to_string`], [`remove_dir`], [`remove_dir_all`], [`remove_file`],
//!   [`rename`], [`set_permissions`], [`symlink_metadata`], [`write`]
//!
//! Each has the signature of its counterpart in `std::fs`, and `symlink` is
//! where std has it, in [`os::unix::fs`](crate::shim::std::os::unix::fs).
//! [`sync_dir`] is an addition.
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
//! - [`sync_dir`], or [`File::sync_all`] on the open directory, syncs the
//!   directory's entries
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
//! ## Syncing a Directory
//!
//! In POSIX C, you sync a directory by opening it and calling `fsync()`:
//! ```c
//! int dir_fd = open("/data", O_RDONLY);
//! fsync(dir_fd);
//! ```
//!
//! On Unix, `std::fs` spells that the same way for a directory as for a file,
//! and so does this shim:
//! ```ignore
//! File::open("/data")?.sync_all()?;
//! ```
//!
//! As on Linux, a directory opens only for reading, and the handle has no
//! bytes to read: it can be synced and asked for its metadata. [`sync_dir`]
//! is shorthand for the two calls, which `std::fs` does not have.
//!
//! ## Testing Patterns
//!
//! Code that is crash-safe on the real filesystem needs only its imports
//! switched:
//!
//! ```ignore
//! #[cfg(test)]
//! use turmoil::fs::shim::std::fs::File;
//! #[cfg(not(test))]
//! use std::fs::File;
//!
//! fn write_durable(path: &Path, data: &[u8]) -> std::io::Result<()> {
//!     let mut file = File::create(path)?;
//!     file.write_all(data)?;
//!     file.sync_all()?;
//!     // The file's name is durable once its directory is synced too. A
//!     // path that is only a name is in the current directory.
//!     let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
//!     File::open(dir.unwrap_or(Path::new(".")))?.sync_all()
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
//! - **The umask is fixed**: Files and directories are created through the
//!   umask that processes usually start with, 022. There is no way to change it.
//!
//! - **No file locking**: `flock()`/`fcntl()` locking is not implemented.
//!
//! - **No ownership**: Every file belongs to user and group 0, and there is
//!   no `chown`.
//!
//! - **Reads leave the access time alone**: As on a filesystem mounted with
//!   `noatime`, only [`File::set_times`] moves it.
//!
//! - **No time before the epoch**: Simulated time starts at the epoch. An
//!   earlier time set on a file becomes the epoch.
//!
//! - **Symlink loops**: Resolving a path follows at most 40 symlinks and then
//!   returns an error, matching POSIX ELOOP behavior.

use crate::errno::{EISDIR, ENOTDIR};
use crate::tree::{FileKind, Ino, Open, Stat, Times, Tree};
use crate::{FileOwner, Fs, FsContext};
use libc::{O_ACCMODE, O_APPEND, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use std::ffi::OsString;
use std::io::{Error, ErrorKind, Read, Result, Seek, SeekFrom, Write};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// What `std` does with a path before any system call sees it: one with a
/// NUL in it is no C string, and fails without a call.
pub(crate) fn c_path<P: AsRef<Path> + ?Sized>(path: &P) -> Result<&Path> {
    let path = path.as_ref();
    match path.as_os_str().as_bytes().contains(&0) {
        true => Err(invalid("file name contained an unexpected NUL byte")),
        false => Ok(path),
    }
}

/// An error that `std` makes itself, before it asks the system for anything.
fn invalid(reason: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, reason)
}

/// Creates a new directory at the provided path.
///
/// Like file creation, directory creation is not durable until the parent
/// directory is synced.
pub fn create_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    DirBuilder::new().create(path)
}

/// Creates a directory and all of its parent components if they are missing.
pub fn create_dir_all<P: AsRef<Path>>(path: P) -> Result<()> {
    DirBuilder::new().recursive(true).create(path)
}

/// Removes an empty directory.
pub fn remove_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = c_path(&path)?;
    FsContext::current(|ctx| ctx.fs.tree.rmdir(path, ctx.now))
}

/// Syncs a directory, making its entries durable.
///
/// This is shorthand for opening the directory and syncing the handle, which
/// is how a directory is synced on Unix and works the same with this shim:
/// ```ignore
/// File::open("/data")?.sync_all()?;
/// ```
///
/// `std::fs` has no function for it, so code that also runs on the real
/// filesystem syncs its directories that way.
///
/// # What It Does
///
/// Equivalent to calling `fsync()` on a directory file descriptor.
/// It makes the following operations durable:
/// - File and directory creations within this directory
/// - File and directory deletions from this directory. A subdirectory goes
///   together with the renames that emptied it
/// - Renames into or out of this directory. A rename is durable as a whole:
///   the name leaves one directory as it arrives in the other. The
///   directories above its new place become durable as far as it takes for
///   a crash that finds the old directory to find the new one, so that the
///   name is never lost
/// - The directory's own creation, if it was recently created and its name
///   does not durably belong to something else
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
pub fn sync_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = c_path(&path)?;
    FsContext::current(|ctx| ctx.fs.tree.sync_dir(path))
}

/// Removes a file from the filesystem.
///
/// Note: The deletion is not durable until the parent directory is synced.
pub fn remove_file<P: AsRef<Path>>(path: P) -> Result<()> {
    let path = c_path(&path)?;
    FsContext::current(|ctx| ctx.fs.tree.unlink(path, ctx.now))
}

/// Renames a file or directory.
///
/// This is atomic - if the destination exists, it will be replaced.
/// The rename is not durable until one of the parent directories is synced,
/// and a crash never leaves it half done.
pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<()> {
    let (from, to) = (c_path(&from)?, c_path(&to)?);
    FsContext::current(|ctx| ctx.fs.tree.rename(from, to, ctx.now))
}

/// Returns `Ok(true)` if the path points at an existing entity, and
/// `Ok(false)` if it points at none.
///
/// This function follows symbolic links, so a symbolic link that leads
/// nowhere does not exist. As with `std::fs::exists`, a path that cannot be
/// looked up at all, such as one that leads through a file, is an error.
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::shim::std::fs::{exists, write, create_dir};
///
/// assert!(!exists("/data")?);
/// create_dir("/data")?;
/// assert!(exists("/data")?);
///
/// write("/data/file.txt", b"hello")?;
/// assert!(exists("/data/file.txt")?);
/// ```
pub fn exists<P: AsRef<Path>>(path: P) -> Result<bool> {
    match stat(path.as_ref(), true) {
        Ok(_) => Ok(true),
        Err(missing) if missing.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn stat(path: &Path, follow: bool) -> Result<Metadata> {
    let path = c_path(path)?;
    FsContext::current(|ctx| {
        let ino = ctx.fs.tree.resolve(path, follow)?;
        Ok(Metadata(ctx.fs.tree.stat(ino)))
    })
}

/// Returns metadata for a file or directory.
///
/// This function follows symbolic links. Use [`symlink_metadata`] to get
/// metadata for the symlink itself.
pub fn metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    stat(path.as_ref(), true)
}

/// Returns an iterator over the entries within a directory.
///
/// The iterator yields instances of `Result<DirEntry>`. Each entry can be
/// inspected for its path, file name, and metadata.
pub fn read_dir<P: AsRef<Path>>(path: P) -> Result<ReadDir> {
    let path = c_path(&path)?;
    let entries = FsContext::current(|ctx| ctx.fs.tree.read_dir(path))?;
    let (root, entries) = (path.into(), entries.into_iter());
    Ok(ReadDir { root, entries })
}

/// Read the entire contents of a file into a bytes vector.
///
/// This is a convenience function for using [`File::open`] and reading the
/// entire file contents.
pub fn read<P: AsRef<Path>>(path: P) -> Result<Vec<u8>> {
    File::open(path)?.read_all()
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
/// As `std::fs::copy` does on Linux, this gives `to` the permissions of
/// `from`, and empties `to` before it reads `from`: a file that is copied
/// onto itself ends up empty.
pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> Result<u64> {
    let source = File::open(from)?;
    let meta = source.metadata()?;
    // As with `std::fs`, the source has to be a regular file.
    if !meta.is_file() {
        let reason = "the source path is neither a regular file nor a symlink to a regular file";
        return Err(invalid(reason));
    }
    let permissions = meta.permissions();
    let mut options = OpenOptions::new();
    options.set_mode(permissions.mode());
    let copy = options.write(true).create(true).truncate(true).open(to)?;
    // The mode a new file is made with goes through the umask, and a file
    // that was there keeps its own, so `std::fs` sets it once more.
    copy.set_permissions(permissions)?;
    let contents = source.read_all()?;
    if !contents.is_empty() {
        copy.write_at_internal(&contents, 0)?;
    }
    Ok(contents.len() as u64)
}

/// Iterator over directory entries.
///
/// Returned by [`read_dir`].
#[derive(Debug)]
pub struct ReadDir {
    /// The path the directory was listed by, which its entries go under.
    root: Arc<Path>,
    entries: std::vec::IntoIter<(OsString, u64)>,
}

impl Iterator for ReadDir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let (name, ino) = self.entries.next()?;
        let root = Arc::clone(&self.root);
        Some(Ok(DirEntry { root, name, ino }))
    }
}

/// An entry inside a directory.
///
/// Returned by iterating over [`ReadDir`].
#[derive(Debug)]
pub struct DirEntry {
    root: Arc<Path>,
    name: OsString,
    /// The inode the entry named when the directory was listed.
    ino: u64,
}

impl DirEntry {
    /// Returns the full path to this entry.
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.name)
    }

    /// Returns the file name of this entry.
    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }

    /// Returns the metadata for this entry.
    ///
    /// This does not follow symlinks - if the entry is a symlink, it returns
    /// metadata for the symlink itself, not the target.
    pub fn metadata(&self) -> Result<Metadata> {
        symlink_metadata(self.path())
    }

    /// Returns the file type for this entry.
    pub fn file_type(&self) -> Result<FileType> {
        // Use symlink_metadata to not follow symlinks
        Ok(self.metadata()?.file_type())
    }
}

impl std::os::unix::fs::DirEntryExt for DirEntry {
    fn ino(&self) -> u64 {
        self.ino
    }
}

/// Representation of a file type.
///
/// Returned by [`DirEntry::file_type`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileType(FileKind);

impl FileType {
    /// Returns true if this file type is a directory.
    pub fn is_dir(&self) -> bool {
        self.0 == FileKind::Dir
    }

    /// Returns true if this file type is a regular file.
    pub fn is_file(&self) -> bool {
        self.0 == FileKind::File
    }

    /// Returns true if this file type is a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.0 == FileKind::Symlink
    }
}

/// The simulation has files, directories and symlinks, and nothing else.
impl std::os::unix::fs::FileTypeExt for FileType {
    fn is_block_device(&self) -> bool {
        false
    }

    fn is_char_device(&self) -> bool {
        false
    }

    fn is_fifo(&self) -> bool {
        false
    }

    fn is_socket(&self) -> bool {
        false
    }
}

/// A simulated file handle.
///
/// Drop-in replacement for `std::fs::File` that stores data in turmoil's
/// per-host simulated filesystem.
#[derive(Debug)]
pub struct File {
    /// The filesystem the file is on. Dropping the file closes its
    /// descriptor there, whichever filesystem the thread has entered.
    owner: FileOwner,
    /// File descriptor used to identify this file in the host's Fs.
    ///
    /// Allocated from a high range (see [`crate::SIM_FD_BASE`]) so it
    /// is safe to expose via [`AsRawFd`](std::os::fd::AsRawFd) — the
    /// io_uring shim resolves these back to files through the host's
    /// open-file table.
    fd: RawFd,
    /// Whether this file bypasses the page cache (O_DIRECT)
    direct_io: bool,
}

impl File {
    /// Attempts to open a file in read-only mode.
    ///
    /// As on Linux, this opens a directory too. Such a handle can be synced
    /// and asked for its metadata, and reading it fails with `IsADirectory`.
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

    /// Creates a new file in read-write mode, and fails if it exists.
    pub fn create_new<P: AsRef<Path>>(path: P) -> Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true).open(path)
    }

    /// Returns a new `OpenOptions`.
    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    /// Runs `f` on the filesystem the file is on. That has to be the one
    /// entered, since the descriptor means nothing on any other.
    pub(crate) fn context<R>(&self, f: impl FnOnce(FsContext<'_>) -> Result<R>) -> Result<R> {
        self.owner.entered()?.context(f)
    }

    /// Runs `f` on the filesystem the file is on, with the inode it has open.
    fn inode<R>(&self, f: impl FnOnce(FsContext<'_>, Ino) -> R) -> Result<R> {
        self.context(|ctx| {
            let ino = ctx.fs.descriptor(self.fd)?.ino;
            Ok(f(ctx, ino))
        })
    }

    /// Everything in the file, read the way `std::fs::read` reads: until
    /// there is no more, however little each read returns.
    fn read_all(&self) -> Result<Vec<u8>> {
        let meta = self.metadata()?;
        // A directory opens, but only a file has bytes to read.
        if meta.is_dir() {
            return Err(EISDIR.into());
        }
        let len = usize::try_from(meta.len()).map_err(|_| ErrorKind::FileTooLarge)?;
        let mut buf = Vec::new();
        let room = buf.try_reserve_exact(len);
        room.map_err(|_| ErrorKind::OutOfMemory)?;
        buf.resize(len, 0);
        let mut filled = 0;
        while filled < len {
            match self.read_at_internal(&mut buf[filled..], filled as u64)? {
                0 => break,
                n => filled += n,
            }
        }
        buf.truncate(filled);
        Ok(buf)
    }

    /// Queries metadata about the file.
    pub fn metadata(&self) -> Result<Metadata> {
        self.inode(|ctx, ino| Metadata(ctx.fs.tree.stat(ino)))
    }

    /// Truncates or extends the file to the specified size.
    pub fn set_len(&self, size: u64) -> Result<()> {
        // `std::fs` takes no size that the system call has no number for.
        let size = i64::try_from(size).map_err(|e| Error::new(ErrorKind::InvalidInput, e))?;
        self.context(|ctx| ctx.fs.set_len(self.fd, size as u64))
    }

    /// Changes the permissions of the file.
    pub fn set_permissions(&self, perm: Permissions) -> Result<()> {
        self.inode(|ctx, ino| ctx.fs.tree.set_mode(ino, perm.mode, ctx.now))
    }

    /// Changes the timestamps of the file. Those that `times` leaves out
    /// stay as they are.
    pub fn set_times(&self, times: FileTimes) -> Result<()> {
        let (accessed, modified) = (times.accessed, times.modified);
        let times = Times::new(accessed.map(since_epoch), modified.map(since_epoch));
        self.inode(|ctx, ino| ctx.fs.tree.set_times(ino, times, ctx.now))
    }

    /// Changes the modification time of the file.
    pub fn set_modified(&self, time: SystemTime) -> Result<()> {
        self.set_times(FileTimes::new().set_modified(time))
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
    /// # Directories
    ///
    /// On an open directory this is what [`sync_dir`] does: it makes the
    /// directory's entries durable.
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
        self.context(|ctx| ctx.fs.sync(self.fd))
    }

    /// Attempts to sync file data to disk, but not all metadata.
    ///
    /// This is equivalent to calling `fdatasync()` on the file descriptor.
    /// It syncs file contents and size, but may not sync all metadata.
    ///
    /// This leaves changes to permissions and timestamps pending.
    /// The directory entry is synced separately with [`sync_dir`].
    pub fn sync_data(&self) -> Result<()> {
        self.context(|ctx| ctx.fs.sync_data(self.fd))
    }

    /// Creates a new `File` that shares the underlying open file with this
    /// one, cursor included: reads, writes and seeks through either handle
    /// move both.
    pub fn try_clone(&self) -> Result<File> {
        let fd = self.context(|ctx| ctx.fs.dup(self.fd))?;
        Ok(File::from_parts(self.owner.clone(), fd, self.direct_io))
    }

    /// Returns whether this file was opened with O_DIRECT (bypasses page cache).
    pub fn is_direct_io(&self) -> bool {
        self.direct_io
    }

    /// Returns the path the file was opened by. Renaming or removing the
    /// file does not change it.
    ///
    /// Returns `None` if the file handle is no longer valid.
    pub fn path(&self) -> Option<PathBuf> {
        let path = |ctx: FsContext<'_>| Ok(ctx.fs.descriptor(self.fd)?.path.clone());
        self.context(path).ok()
    }

    /// Internal read implementation.
    pub(crate) fn read_at_internal(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.context(|ctx| ctx.fs.read_at(self.fd, buf, offset))
    }

    /// Internal write implementation.
    pub(crate) fn write_at_internal(&self, buf: &[u8], offset: u64) -> Result<usize> {
        let written = self.context(|ctx| ctx.fs.write_at(self.fd, buf, offset))?;
        Ok(written.len)
    }

    /// Create a File from internal state (used by OpenOptions).
    fn from_parts(owner: FileOwner, fd: RawFd, direct_io: bool) -> Self {
        Self {
            owner,
            fd,
            direct_io,
        }
    }
}

impl Read for &File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.context(|ctx| ctx.fs.read(self.fd, buf))
    }
}

impl Write for &File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.context(|ctx| ctx.fs.write(self.fd, buf))
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Seek for &File {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.context(|ctx| ctx.fs.seek(self.fd, pos))
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (&*self).read(buf)
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> Result<()> {
        (&*self).flush()
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        (&*self).seek(pos)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        self.owner.close(self.fd);
    }
}

/// Exposes the simulated file descriptor.
///
/// The returned [`RawFd`] is allocated from a high range
/// ([`crate::SIM_FD_BASE`]) so it cannot collide with any real OS
/// fd. It is meaningful only inside the turmoil simulation that minted
/// it; passing it to a real syscall will reliably fail with `EBADF`.
///
/// This impl exists so consumers can hand the fd to the
/// [`crate::io_uring`] shim.
impl std::os::fd::AsRawFd for File {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

/// Metadata information about a file.
#[derive(Debug, Clone)]
pub struct Metadata(Stat);

impl Metadata {
    /// Returns the size of the file in bytes.
    #[allow(clippy::len_without_is_empty)] // is_empty() is not part of std::fs::Metadata
    pub fn len(&self) -> u64 {
        self.0.len
    }

    /// Returns true if this metadata is for a regular file.
    pub fn is_file(&self) -> bool {
        self.file_type().is_file()
    }

    /// Returns true if this metadata is for a directory.
    pub fn is_dir(&self) -> bool {
        self.file_type().is_dir()
    }

    /// Returns true if this metadata is for a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.file_type().is_symlink()
    }

    /// Returns the file type for this metadata.
    pub fn file_type(&self) -> FileType {
        FileType(self.0.kind)
    }

    /// Returns the last modification time.
    ///
    /// This is updated when file contents are modified (writes, truncate).
    pub fn modified(&self) -> Result<SystemTime> {
        Ok(UNIX_EPOCH + self.0.meta.mtime)
    }

    /// Returns the last access time.
    ///
    /// Note: turmoil simulates noatime behavior, so reads leave this alone.
    /// It is the creation time until [`File::set_times`] sets it.
    pub fn accessed(&self) -> Result<SystemTime> {
        Ok(UNIX_EPOCH + self.0.meta.atime)
    }

    /// Returns the creation time (birthtime).
    ///
    /// Note: Not all platforms support creation time. On platforms that don't
    /// support it, `std::fs::Metadata::created()` returns an error. Our
    /// simulation always tracks it.
    pub fn created(&self) -> Result<SystemTime> {
        Ok(UNIX_EPOCH + self.0.meta.crtime)
    }

    /// Returns the permissions of the file. As with `std::fs`, their mode
    /// holds the type of the file as well.
    pub fn permissions(&self) -> Permissions {
        Permissions::from_mode(self.mode())
    }
}

impl std::os::unix::fs::MetadataExt for Metadata {
    fn dev(&self) -> u64 {
        0 // Simulated device
    }

    fn ino(&self) -> u64 {
        self.0.ino
    }

    fn mode(&self) -> u32 {
        // Include file type bits: regular file (0o100000), directory (0o40000), symlink (0o120000)
        let type_bits = match self.0.kind {
            FileKind::Symlink => 0o120000,
            FileKind::Dir => 0o040000,
            FileKind::File => 0o100000,
        };
        type_bits | self.0.meta.mode
    }

    fn nlink(&self) -> u64 {
        self.0.nlink
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
        self.0.len
    }

    fn atime(&self) -> i64 {
        self.0.meta.atime.as_secs() as i64
    }

    fn atime_nsec(&self) -> i64 {
        self.0.meta.atime.subsec_nanos() as i64
    }

    fn mtime(&self) -> i64 {
        self.0.meta.mtime.as_secs() as i64
    }

    fn mtime_nsec(&self) -> i64 {
        self.0.meta.mtime.subsec_nanos() as i64
    }

    fn ctime(&self) -> i64 {
        self.0.meta.ctime.as_secs() as i64
    }

    fn ctime_nsec(&self) -> i64 {
        self.0.meta.ctime.subsec_nanos() as i64
    }

    fn blksize(&self) -> u64 {
        4096 // Simulated block size
    }

    fn blocks(&self) -> u64 {
        self.0.blocks
    }
}

/// Options and flags for opening files.
///
/// Drop-in replacement for `std::fs::OpenOptions`.
///
/// The custom flags of `OpenOptionsExt` are the host's flags for `open(2)`.
/// Those that mean something to the simulation do what they do on Linux:
/// `O_APPEND`, `O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_NOFOLLOW`, `O_DIRECTORY`,
/// `O_DIRECT`, `O_SYNC` and `O_DSYNC`. The rest are ignored.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    /// Unix mode bits for file creation, before the umask (default: 0o666)
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
            mode: 0o666,
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
    /// On real systems, you would use `custom_flags(libc::O_DIRECT)` on
    /// Linux, which this shim takes to mean the same, or `fcntl(F_NOCACHE)`
    /// on macOS.
    ///
    /// When enabled, reads and writes bypass the simulated page cache,
    /// always incurring full I/O latency. Additionally, all reads and writes
    /// enforce alignment requirements: the buffer pointer, file offset, and
    /// buffer length must each be a multiple of `direct_io_alignment` (default
    /// 512 bytes). Misaligned operations fail with `InvalidInput`, matching
    /// the `EINVAL` behavior of the Linux kernel.
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
    pub(crate) fn set_custom_flags(&mut self, flags: i32) {
        self.custom_flags = flags;
    }

    /// Opens a file at `path` with the options specified by `self`.
    ///
    /// As on Linux, a directory opens only for reading. Options that could
    /// change or create a file fail on one with `IsADirectory`, or with
    /// `AlreadyExists` if they are to create a new file.
    pub fn open<P: AsRef<Path>>(&self, path: P) -> Result<File> {
        let path = c_path(&path)?;
        let mut how = Open::from_flags(self.flags()?, self.mode)?;
        how.direct |= self.direct_io;
        let fs = Fs::current();
        let fd = fs.context(|ctx| ctx.fs.open(path, &how))?;
        Ok(File::from_parts(FileOwner::new(&fs), fd, how.direct))
    }

    /// The flags that `std::fs` opens with: the access mode and the
    /// creation mode it makes of the options, and the custom flags.
    fn flags(&self) -> Result<i32> {
        self.validate()?;
        let flag = |on: bool, flag: i32| [0, flag][on as usize];
        let access = match (self.read, self.write || self.append) {
            (true, false) => O_RDONLY,
            (true, true) => O_RDWR,
            (false, _) => O_WRONLY,
        };
        let creation = match self.create_new {
            true => O_CREAT | O_EXCL,
            false => flag(self.create, O_CREAT) | flag(self.truncate, O_TRUNC),
        };
        let custom = self.custom_flags & !O_ACCMODE;
        Ok(access | flag(self.append, O_APPEND) | creation | custom)
    }

    /// Refuses the combinations that `std::fs` refuses, which it does
    /// before it asks the filesystem for anything.
    fn validate(&self) -> Result<()> {
        let makes = self.create || self.create_new || self.truncate;
        let inaccessible = "must specify at least one of read, write, or append access";
        let unwritable = "creating or truncating a file requires write or append access";
        let reason = match (self.write, self.append) {
            (false, false) if makes => unwritable,
            (false, false) if !self.read => inaccessible,
            (_, true) if self.truncate && !self.create_new => unwritable,
            _ => return Ok(()),
        };
        Err(invalid(reason))
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
    /// Returns true if these permissions describe a readonly (unwritable)
    /// file: one that nobody may write, as with `std::fs`.
    pub fn readonly(&self) -> bool {
        self.mode & 0o222 == 0
    }

    /// Modifies the readonly flag for this set of permissions. As with
    /// `std::fs`, this is `chmod a-w` or `chmod a+w`: clearing the flag
    /// lets everybody write.
    pub fn set_readonly(&mut self, readonly: bool) {
        match readonly {
            true => self.mode &= !0o222,
            false => self.mode |= 0o222,
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
    let path = c_path(&path)?;
    FsContext::current(|ctx| {
        let ino = ctx.fs.tree.resolve(path, true)?;
        ctx.fs.tree.set_mode(ino, perm.mode, ctx.now);
        Ok(())
    })
}

/// Representation of the various timestamps on a file.
///
/// Drop-in replacement for `std::fs::FileTimes`.
#[derive(Copy, Clone, Debug, Default)]
#[must_use = "must be applied to a file via `File::set_times` to have any effect"]
pub struct FileTimes {
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
}

impl FileTimes {
    /// Creates a new `FileTimes` with no times set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the last access time of a file.
    pub fn set_accessed(mut self, t: SystemTime) -> Self {
        self.accessed = Some(t);
        self
    }

    /// Sets the last modified time of a file.
    pub fn set_modified(mut self, t: SystemTime) -> Self {
        self.modified = Some(t);
        self
    }
}

/// Simulated time starts at the epoch, and the filesystem holds no time
/// before it. As Linux does with a time a filesystem cannot hold, it keeps
/// the nearest one it can.
fn since_epoch(time: SystemTime) -> Duration {
    time.duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// Reads the target of a symbolic link.
pub fn read_link<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let path = c_path(&path)?;
    FsContext::current(|ctx| ctx.fs.tree.read_link(path))
}

/// Returns metadata for a path without following symlinks.
///
/// If `path` is a symlink, this returns metadata for the symlink itself,
/// not the target.
pub fn symlink_metadata<P: AsRef<Path>>(path: P) -> Result<Metadata> {
    stat(path.as_ref(), false)
}

/// Creates a hard link on the filesystem.
///
/// The `link` path will be a hard link to the `original` file.
/// Both paths will refer to the same file content.
pub fn hard_link<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let (original, link) = (c_path(&original)?, c_path(&link)?);
    FsContext::current(|ctx| ctx.fs.tree.hard_link(original, link, ctx.now))
}

/// A builder for creating directories with options.
///
/// Drop-in replacement for `std::fs::DirBuilder`.
#[derive(Debug)]
pub struct DirBuilder {
    recursive: bool,
    /// Unix mode bits for the directories, before the umask (default: 0o777)
    mode: u32,
}

impl Default for DirBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DirBuilder {
    /// Creates a new DirBuilder with default options.
    pub fn new() -> Self {
        Self {
            recursive: false,
            mode: 0o777,
        }
    }

    /// Sets the option for recursive directory creation.
    pub fn recursive(&mut self, recursive: bool) -> &mut Self {
        self.recursive = recursive;
        self
    }

    /// Creates the directory at the given path.
    pub fn create<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let (path, mode) = (c_path(&path)?, self.mode);
        FsContext::current(|ctx| match self.recursive {
            true => mkdir_all(&mut ctx.fs.tree, path, mode, ctx.now),
            false => ctx.fs.tree.mkdir(path, mode, ctx.now),
        })
    }
}

/// Makes the directory at `path` and those on the way to it that are
/// missing, the way `std::fs` goes about it: from the path up, and back.
fn mkdir_all(tree: &mut Tree, path: &Path, mode: u32, now: Duration) -> Result<()> {
    // As with `std::fs`, there is nothing to create for the empty path.
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let made = match (tree.mkdir(path, mode, now), path.parent()) {
        (Err(missing), Some(parent)) if missing.kind() == ErrorKind::NotFound => {
            mkdir_all(tree, parent, mode, now)?;
            tree.mkdir(path, mode, now)
        }
        (made, _) => made,
    };
    // Whatever went wrong, a directory that is there is what was asked for.
    match made {
        Err(_) if tree.resolve(path, true).is_ok_and(|ino| tree.is_dir(ino)) => Ok(()),
        made => made,
    }
}

impl std::os::unix::fs::DirBuilderExt for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }
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
pub fn canonicalize<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let path = c_path(&path)?;
    FsContext::current(|ctx| ctx.fs.tree.canonicalize(path))
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
    let path = c_path(&path)?;
    FsContext::current(|ctx| {
        let (tree, now) = (&mut ctx.fs.tree, ctx.now);
        // The way `std::fs` goes about it on Linux: a symlink is removed
        // rather than followed, unless a slash follows it. A directory is
        // emptied and then removed by the path, which may fail by then, or
        // find nothing there.
        let ino = tree.resolve(path, false)?;
        match tree.stat(ino).kind {
            FileKind::Symlink => tree.unlink(path, now),
            FileKind::File => Err(ENOTDIR.into()),
            FileKind::Dir => {
                tree.clear(ino, now);
                match tree.rmdir(path, now) {
                    Err(gone) if gone.kind() == ErrorKind::NotFound => Ok(()),
                    removed => removed,
                }
            }
        }
    })
}
