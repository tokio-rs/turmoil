//! The real arm: `std::fs`, on the filesystem of the machine.

pub use std::fs::{
    canonicalize, copy, create_dir, create_dir_all, exists, hard_link, metadata, read, read_dir,
    read_link, read_to_string, remove_dir, remove_dir_all, remove_file, rename, set_permissions,
    symlink_metadata, write, DirBuilder, DirEntry, File, FileTimes, OpenOptions, Permissions,
};
pub use std::os::unix::fs::symlink;

#[path = "cases.rs"]
pub mod cases;
