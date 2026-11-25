//! Unix-specific filesystem extensions.
//!
//! Implements the real `std::os::unix::fs::{FileExt, OpenOptionsExt}` traits
//! for the shimmed [`File`] and [`OpenOptions`] types.
//!
//! Users import these traits from `std::os::unix::fs` as normal - they work
//! with our shimmed types because we implement the real traits. What that
//! module has besides traits is here: [`symlink`].
//!
//! [`File`]: crate::shim::std::fs::File
//! [`OpenOptions`]: crate::shim::std::fs::OpenOptions

use crate::shim::std::fs::{c_path, File, OpenOptions};
use crate::FsContext;
use std::io::Result;
use std::path::Path;

/// Creates a new symbolic link on the filesystem.
///
/// The `link` path will be a symbolic link pointing to the `original` path.
pub fn symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> Result<()> {
    let (original, link) = (c_path(&original)?, c_path(&link)?);
    FsContext::current(|ctx| ctx.fs.tree.symlink(original, link, ctx.now))
}

#[cfg(unix)]
impl std::os::unix::fs::FileExt for File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.read_at_internal(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<usize> {
        self.write_at_internal(buf, offset)
    }
}

#[cfg(unix)]
impl std::os::unix::fs::OpenOptionsExt for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        self.set_mode(mode);
        self
    }

    fn custom_flags(&mut self, flags: i32) -> &mut Self {
        self.set_custom_flags(flags);
        self
    }
}
