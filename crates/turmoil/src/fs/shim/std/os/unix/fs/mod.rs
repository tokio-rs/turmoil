//! Unix-specific filesystem extensions.
//!
//! Implements the real `std::os::unix::fs::{FileExt, OpenOptionsExt}` traits
//! for the shimmed [`File`] and [`OpenOptions`] types.
//!
//! Users import these traits from `std::os::unix::fs` as normal - they work
//! with our shimmed types because we implement the real traits.
//!
//! [`File`]: crate::fs::shim::std::fs::File
//! [`OpenOptions`]: crate::fs::shim::std::fs::OpenOptions

use crate::fs::shim::std::fs::{File, OpenOptions};
use std::io::Result;

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
