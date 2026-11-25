//! The simulated arm: the shim, on the filesystem of a simulated host.

pub use turmoil::fs::shim::std::fs::{
    canonicalize, copy, create_dir, create_dir_all, exists, hard_link, metadata, read, read_dir,
    read_link, read_to_string, remove_dir, remove_dir_all, remove_file, rename, set_permissions,
    symlink_metadata, write, DirBuilder, DirEntry, File, FileTimes, OpenOptions, Permissions,
};
pub use turmoil::fs::shim::std::os::unix::fs::symlink;

#[path = "cases.rs"]
pub mod cases;
