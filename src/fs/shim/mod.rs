//! Shim module providing drop-in replacements for filesystem types.
//!
//! This module provides simulated versions of standard library and tokio
//! filesystem types that store data in turmoil's per-host virtual filesystem
//! instead of the real filesystem.
//!
//! # What is shimmed
//!
//! ## std::fs
//!
//! **Structs** (custom implementations that mirror std behavior):
//! - [`std::fs::File`] - Simulated file handle
//! - [`std::fs::OpenOptions`] - Options for opening files
//! - [`std::fs::Metadata`] - File metadata
//!
//! **Free functions** (operate on the simulated filesystem):
//! - [`std::fs::create_dir`], [`std::fs::create_dir_all`]
//! - [`std::fs::remove_dir`], [`std::fs::remove_dir_all`], [`std::fs::remove_file`]
//! - [`std::fs::rename`], [`std::fs::canonicalize`]
//!
//! ## tokio::fs
//!
//! Async wrappers around the std::fs shims:
//! - [`tokio::fs::File`] - Async file handle with `AsyncRead`/`AsyncWrite`/`AsyncSeek`
//! - [`tokio::fs::OpenOptions`] - Async options for opening files
//! - All free functions: [`tokio::fs::read`], [`tokio::fs::write`], etc.
//!
//! # What is NOT shimmed
//!
//! **Traits** - Our shimmed structs implement the real traits:
//! - `std::io::Read`, `std::io::Write`, `std::io::Seek` on std `File`
//! - `std::os::unix::fs::FileExt` on std `File` (Unix only)
//! - `tokio::io::AsyncRead`, `AsyncWrite`, `AsyncSeek` on tokio `File`
//!
//! # Usage
//!
//! Replace your imports with turmoil shim imports:
//!
//! ```ignore
//! // Instead of:
//! // use std::fs::{File, OpenOptions};
//! // use tokio::fs;
//!
//! // Use:
//! use turmoil::fs::shim::std::fs::{File, OpenOptions};
//! use turmoil::fs::shim::tokio::fs as tokio_fs;
//! ```

pub mod std;
pub mod tokio;
