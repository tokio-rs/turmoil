//! Maps and sets keyed by the numbers that the simulation hands out.

use indexmap::{IndexMap, IndexSet};
use std::hash::{BuildHasherDefault, Hasher};

/// Hashes the ids of inodes, directory entries and descriptors. They are
/// counters, which a multiplication spreads as well as a hash that has to
/// withstand chosen keys, for a fraction of its cost: every component of a
/// path is looked up by id more than once.
#[derive(Default)]
pub(crate) struct IdHasher(u64);

impl IdHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for IdHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.add(byte.into());
        }
    }

    fn write_u64(&mut self, id: u64) {
        self.add(id);
    }

    fn write_i32(&mut self, id: i32) {
        self.add(id as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type IdMap<K, V> = IndexMap<K, V, BuildHasherDefault<IdHasher>>;
pub(crate) type IdSet<K> = IndexSet<K, BuildHasherDefault<IdHasher>>;
