//! Tests for simulated filesystem.
//!
//! Requires the `unstable-fs` feature to be enabled.
//!
//! Test modules:
//! - `basic`: Basic file operations (read, write, truncate, etc.)
//! - `cache`: Page cache simulation
//! - `dirs`: Directory operations (create, remove, read_dir, rename)
//! - `durability`: Crash consistency and sync tests
//! - `links`: Symlinks and hard links
//! - `metadata`: File/directory metadata and timestamps
//! - `threads`: Worker thread filesystem access
//! - `tokio`: Async filesystem operations

#![cfg(feature = "unstable-fs")]

mod basic;
mod cache;
mod dirs;
mod durability;
mod links;
mod metadata;
mod threads;
mod tokio;
