//! Authoritative filesystem implementation.
//!
//! TODO: lift the per-host namespace, inode/file table, pending/synced
//! durability state, crash recovery, and O_DIRECT alignment rules out of
//! `turmoil::fs` into this module.
