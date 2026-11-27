//! Simulated filesystem for turmoil.
//!
//! This crate provides a drop-in replacement for `std::fs` and
//! `std::os::unix::fs` that simulates filesystem behavior with
//! durability testing support.
//!
//! # Usage
//!
//! Replace your type imports with turmoil shim types, but keep std traits:
//! ```ignore
//! // Before
//! use std::fs::{File, OpenOptions};
//! use std::os::unix::fs::{FileExt, OpenOptionsExt};
//!
//! // After (types from turmoil, traits from std)
//! use turmoil::fs::shim::std::fs::{File, OpenOptions};
//! use std::os::unix::fs::{FileExt, OpenOptionsExt};  // Real traits work with our types
//! ```
//!
//! # Durability Model
//!
//! - Writes and truncations are pending until the file is synced with
//!   `sync_all()`, or randomly based on [`FsConfig::sync_probability`]
//! - Creations and removals are pending until the directory holding the
//!   name is synced, with `sync_dir()` or `sync_all()` on the open directory
//! - A rename is pending until the directory the name was durable in, or
//!   the one it lives in now, is synced. It then becomes durable as a whole:
//!   a crash never finds the file under both names, and never loses a file
//!   it would have found under the old one
//! - On crash (`sim.crash()`):
//!   - Pending writes and pending names are lost; durable data survives
//!   - A file that no durable name reaches is gone, synced or not
//! - Each host has its own isolated filesystem namespace
//!
//! # Setup
//!
//! ```ignore
//! let fs = Fs::builder().seed(42).sync_probability(0.1).build();
//! let _guard = fs.enter();
//! // shim operations now route through this Fs
//! ```
//!
//! For embedding into a multi-host harness (e.g. `turmoil`), store an
//! `Fs` per host. Before each tick, advance time via [`Fs::set_now`]
//! and call `fs.enter()`. Cloning an `Fs` is cheap (shared state).

mod errno;
mod hash;
pub mod shim;
mod tree;

pub use tree::Ino;

use errno::{EBADF, EFBIG, EINVAL, EIO, ENOSPC};
use hash::{IdMap, IdSet};
use rand::rngs::SmallRng;
use rand::{Rng, RngCore, SeedableRng};
use rand_distr::{Distribution, Exp};
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::fmt;
use std::io::{self, ErrorKind, SeekFrom};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tree::{Change, Open, Tree, WriteSync};

/// Type alias for the optional corruption hook stored on [`FsState`].
pub type CorruptionHook = Option<CorruptionCallback>;
type CorruptionCallback = Arc<dyn Fn(&FsCorruption) + Send + Sync>;

/// The longest a file gets. Offsets and lengths are signed on Linux, so a
/// greater one is no valid argument.
const MAX_FILE_LEN: u64 = i64::MAX as u64;

/// Base value for sim-allocated file descriptors.
///
/// Far enough above any plausible real fd (typical kernel max is ~1M) that
/// even if a sim fd accidentally leaked into a real syscall it would
/// reliably error with `EBADF` rather than silently address a real fd.
/// Stays inside `i32` to fit `RawFd`.
pub const SIM_FD_BASE: RawFd = 1 << 30;

// ─── Enter pattern ──────────────────────────────────────────────────
//
// `enter(&arc)` stores the `Arc<Mutex<FsState>>` in a thread-local.
// While the returned guard is alive, `FsContext::current` reads from
// that thread-local — no globals, no installed function pointers,
// just lexically-scoped access. The embedder (`turmoil`) calls
// `enter` once per host tick.
//
// Same shape `turmoil-net` uses for its CURRENT thread-local: the
// state lives in the entered struct, not in a global registry.
//
// Simulated time and the corruption hook live on `FsState` directly —
// no external per-tick injection. The embedder advances
// time with `Fs::set_now` before each tick; the hook is set at build time.

thread_local! {
    /// `Arc<Mutex<FsState>>` of the entered fs. Locked on each
    /// `FsContext::current` call.
    static CURRENT_FS_ARC: RefCell<Option<Arc<Mutex<FsState>>>> = const { RefCell::new(None) };
}

// Nested subsystem calls share the outer lock scope's notifications.
scoped_thread_local!(static CORRUPTIONS: RefCell<Vec<(CorruptionCallback, FsCorruption)>>);

/// Guard returned by [`enter`]. While alive, the entered `Fs` is the
/// current one for [`FsContext::current`] on this thread.
///
/// `enter` does NOT hold the `Fs` mutex for the guard's lifetime —
/// see the docstring on [`enter`] for why. The mutex is locked on
/// each `FsContext::current` call.
///
/// `enter` calls nest. Dropping the guard restores the previous
/// current `Fs` (if any).
#[must_use = "the entered Fs is only current while the guard is held"]
pub struct FsEnterGuard {
    prev_arc: Option<Arc<Mutex<FsState>>>,
}

impl Drop for FsEnterGuard {
    fn drop(&mut self) {
        CURRENT_FS_ARC.with(|c| *c.borrow_mut() = self.prev_arc.take());
    }
}

/// Mark `arc`'s `FsState` as the current one for [`FsContext::current`]
/// on this thread.
///
/// While the guard is alive, shim operations on this thread
/// (`turmoil_fs::shim::std::fs::*` etc.) route through this `Fs`. The
/// embedder (typically `turmoil`) calls `enter` once per host tick.
///
/// **Lock-on-demand:** `enter` does not hold the mutex on `arc`. Each
/// `FsContext::current` call inside the guard locks the mutex for the
/// duration of the closure body, then releases it. This sidesteps a
/// deadlock with OS threads using cloned [`Fs`] values that synchronously
/// block the main sim thread (e.g. `std::thread::spawn` + `join`).
///
/// `enter` calls nest. Dropping the guard restores the previous
/// current `Fs` (if any).
pub fn enter(arc: &Arc<Mutex<FsState>>) -> FsEnterGuard {
    let prev_arc = CURRENT_FS_ARC.with(|c| c.borrow_mut().replace(Arc::clone(arc)));
    FsEnterGuard { prev_arc }
}

/// Context for filesystem operations, bundling Fs state with time.
///
/// Borrowed handle to the current host's filesystem state. Made `pub`
/// so sister crates (notably `turmoil-io-uring`) can call
/// [`FsContext::current`] when they need `&mut FsState`.
/// Published-but-unstable surface — not stable for end users.
///
/// `now` is read from `fs.now` at the time the context is acquired.
pub struct FsContext<'a> {
    /// The filesystem state
    pub fs: &'a mut FsState,
    /// Current simulated time (snapshot of `fs.now` when context was acquired)
    pub now: Duration,
}

impl FsContext<'_> {
    /// Run `f` with the current host's filesystem context.
    ///
    /// The current filesystem is thread-local, so each worker thread
    /// must call [`Fs::enter`] before using filesystem shims.
    ///
    /// # Panics
    ///
    /// Panics if called without an entered [`Fs`] on this thread.
    pub fn current<R>(f: impl FnOnce(FsContext<'_>) -> R) -> R {
        Fs::current().context(f)
    }

    /// Run `f` if a filesystem is entered on this thread.
    pub fn current_if_set(f: impl FnOnce(FsContext<'_>)) {
        if let Some(fs) = Fs::try_current() {
            fs.context(f);
        }
    }
}

/// Trigger type for silent data corruption events.
///
/// This is fired when the filesystem simulation corrupts read data.
/// Use with [`crate::barriers::Barrier`] to observe or control corruption:
///
/// ```ignore
/// use turmoil::barriers::Barrier;
/// use turmoil::fs::FsCorruption;
///
/// let mut barrier = Barrier::new(|c: &FsCorruption| {
///     c.path.ends_with("data.db")
/// });
///
/// // Run simulation until corruption occurs
/// let corruption = barrier.wait().await.unwrap();
/// println!("Corrupted {} bytes at offset {}", corruption.len, corruption.offset);
/// ```
#[derive(Debug, Clone)]
pub struct FsCorruption {
    /// The path the corrupted file was opened by, whatever its name is now
    pub path: PathBuf,
    /// Offset within the file where corruption occurred
    pub offset: u64,
    /// Number of bytes that were corrupted
    pub len: usize,
}

/// Configure I/O latency for filesystem operations.
///
/// Only applies to async (tokio::fs) operations. Sync operations complete
/// immediately since there's no way to sleep in simulated time for sync code.
///
/// # Defaults
///
/// - `min_latency`: 50µs (SSD-like)
/// - `max_latency`: 5ms
/// - `distribution`: Exponential with λ=5.0
#[derive(Debug, Clone)]
pub struct IoLatency {
    /// Minimum latency for I/O operations
    pub(crate) min_latency: Duration,
    /// Maximum latency for I/O operations
    pub(crate) max_latency: Duration,
    /// Probability distribution of latency within the range above
    pub(crate) distribution: Exp<f64>,
}

impl Default for IoLatency {
    fn default() -> Self {
        Self {
            min_latency: Duration::from_micros(50),
            max_latency: Duration::from_millis(5),
            distribution: Exp::new(5.0).unwrap(),
        }
    }
}

impl IoLatency {
    /// Set the minimum I/O latency.
    ///
    /// Default: 50µs
    pub fn min_latency(&mut self, value: Duration) -> &mut Self {
        self.min_latency = value;
        self
    }

    /// Set the maximum I/O latency.
    ///
    /// Default: 5ms
    pub fn max_latency(&mut self, value: Duration) -> &mut Self {
        self.max_latency = value;
        self
    }

    /// Set the latency distribution curve.
    ///
    /// Higher values produce more latencies near the minimum.
    /// Default: 5.0
    pub fn distribution(&mut self, lambda: f64) -> &mut Self {
        self.distribution = Exp::new(lambda).expect("lambda must be positive");
        self
    }
}

/// Page cache configuration.
///
/// Simulates OS page cache behavior for I/O latency:
/// - **Cache hits**: Near-instant (memory speed)
/// - **Cache misses**: Full I/O latency
/// - **O_DIRECT**: Bypasses cache entirely
///
/// # Defaults
///
/// - `page_size`: 4096 bytes
/// - `max_pages`: 256 (1MB total cache)
/// - `random_eviction_probability`: 0.0 (pure LRU)
#[derive(Debug, Clone)]
pub struct PageCacheConfig {
    /// Page size in bytes (default: 4096)
    pub(crate) page_size: u64,
    /// Maximum number of pages in cache (default: 256 = 1MB with 4KB pages)
    pub(crate) max_pages: usize,
    /// Probability of random eviction on cache access (0.0-1.0, default: 0.0)
    /// Use this to add chaos/unpredictability to caching behavior
    pub(crate) random_eviction_probability: f64,
}

impl Default for PageCacheConfig {
    fn default() -> Self {
        Self {
            page_size: 4096,
            max_pages: 256,
            random_eviction_probability: 0.0,
        }
    }
}

impl PageCacheConfig {
    /// Set the page size in bytes.
    ///
    /// Default: 4096 (4KB)
    pub fn page_size(&mut self, size: u64) -> &mut Self {
        assert!(size > 0, "page_size must be positive");
        self.page_size = size;
        self
    }

    /// Set the maximum number of pages in cache.
    ///
    /// Default: 256 (1MB with 4KB pages)
    pub fn max_pages(&mut self, count: usize) -> &mut Self {
        self.max_pages = count;
        self
    }

    /// Set the probability of random eviction on cache access.
    ///
    /// When > 0, pages may be randomly evicted even if they would
    /// normally be a cache hit. This adds chaos for testing cache-
    /// sensitive code paths.
    ///
    /// Default: 0.0 (pure LRU, no random eviction)
    pub fn random_eviction_probability(&mut self, prob: f64) -> &mut Self {
        assert!(
            (0.0..=1.0).contains(&prob),
            "random_eviction_probability must be between 0.0 and 1.0"
        );
        self.random_eviction_probability = prob;
        self
    }
}

/// Configuration for the simulated filesystem.
///
/// Use builder-style methods to configure filesystem behavior:
///
/// ```ignore
/// let sim = turmoil::Builder::new()
///     .fs()
///         .sync_probability(0.1)
///         .capacity(1024 * 1024)
///         .io_error_probability(0.01)
///     .build();
/// ```
///
/// ## Defaults
///
/// - `sync_probability`: 0.0 (writes only durable on explicit fsync)
/// - `capacity`: None (unlimited)
/// - `io_error_probability`: 0.0 (no random I/O errors)
/// - `corruption_probability`: 0.0 (no silent corruption)
/// - `short_read_probability`: 0.0 (positional reads always return the full count)
/// - `noatime`: true (access time not updated on reads)
/// - `direct_io_alignment`: 512 (minimum kernel alignment for O_DIRECT)
/// - `block_size`: None (writes are atomic, no torn writes)
/// - `io_latency`: None (instant operations)
/// - `page_cache`: None (no page cache simulation)
#[derive(Debug, Clone)]
pub struct FsConfig {
    /// Probability that writes are randomly synced to durable storage (0.0 - 1.0)
    pub(crate) sync_probability: f64,
    /// Disk capacity in bytes (None = unlimited)
    pub(crate) capacity: Option<u64>,
    /// Probability of I/O errors (0.0 - 1.0)
    pub(crate) io_error_probability: f64,
    /// Probability of silent data corruption on reads (0.0 - 1.0)
    pub(crate) corruption_probability: f64,
    /// Probability that a positional read returns a short count (0.0 - 1.0)
    pub(crate) short_read_probability: f64,
    /// Whether to use noatime semantics (atime not updated on reads)
    pub(crate) noatime: bool,
    /// Required alignment in bytes for O_DIRECT I/O (buffer pointer, offset, and length)
    pub(crate) direct_io_alignment: u64,
    /// Block size for torn write simulation (None = atomic writes)
    pub(crate) block_size: Option<u64>,
    /// I/O latency configuration (None = instant operations)
    pub(crate) io_latency: Option<IoLatency>,
    /// Page cache configuration (None = no cache simulation)
    pub(crate) page_cache: Option<PageCacheConfig>,
}

impl Default for FsConfig {
    fn default() -> Self {
        Self {
            sync_probability: 0.0,
            capacity: None,
            io_error_probability: 0.0,
            corruption_probability: 0.0,
            short_read_probability: 0.0,
            noatime: true,
            direct_io_alignment: 512,
            block_size: None,
            io_latency: None,
            page_cache: None,
        }
    }
}

impl FsConfig {
    /// Set the probability that filesystem writes are randomly synced to
    /// durable storage (0.0 - 1.0).
    ///
    /// Default: 0.0 (writes only become durable on explicit fsync).
    ///
    /// This is useful for testing crash consistency - with a non-zero
    /// probability, some writes may survive a crash even without fsync.
    pub fn sync_probability(&mut self, value: f64) -> &mut Self {
        assert!(
            (0.0..=1.0).contains(&value),
            "sync_probability must be between 0.0 and 1.0"
        );
        self.sync_probability = value;
        self
    }

    /// Set the disk capacity in bytes per host.
    ///
    /// Default: None (unlimited).
    ///
    /// When set, writes that would exceed the capacity will fail with ENOSPC.
    pub fn capacity(&mut self, bytes: u64) -> &mut Self {
        self.capacity = Some(bytes);
        self
    }

    /// Set the probability of I/O errors on reads/writes (0.0 - 1.0).
    ///
    /// Default: 0.0 (no errors).
    ///
    /// When set, reads and writes may randomly fail with EIO.
    pub fn io_error_probability(&mut self, value: f64) -> &mut Self {
        assert!(
            (0.0..=1.0).contains(&value),
            "io_error_probability must be between 0.0 and 1.0"
        );
        self.io_error_probability = value;
        self
    }

    /// Set the probability of silent data corruption on reads (0.0 - 1.0).
    ///
    /// Default: 0.0 (no corruption).
    ///
    /// When set, reads may randomly return corrupted data without any error.
    /// This simulates real disk behavior where hardware can silently return
    /// bad data (bit flips, misdirected reads, etc.).
    ///
    /// Use with [`crate::barriers`] to test corruption handling:
    /// ```ignore
    /// let mut barrier = Barrier::new(|t: &FsCorruption| t.path == Path::new("/data.db"));
    /// // ... run simulation ...
    /// let corruption = barrier.wait().await;
    /// ```
    pub fn corruption_probability(&mut self, value: f64) -> &mut Self {
        assert!(
            (0.0..=1.0).contains(&value),
            "corruption_probability must be between 0.0 and 1.0"
        );
        self.corruption_probability = value;
        self
    }

    /// Set the probability that a positional read returns a short count (0.0 - 1.0).
    ///
    /// Default: 0.0 (reads always return the full requested count, subject to EOF).
    ///
    /// When set, reads through `File::read`, `File::read_at`, and their async
    /// equivalents may randomly return fewer bytes than requested, even when
    /// more data is available. This simulates the short-read behavior of
    /// `pread` on real kernels (e.g., `EINTR` interrupting a blocking syscall).
    /// O_DIRECT reads are not exempt — signals can short-circuit them too.
    ///
    /// The tail of the buffer past the returned count is zeroed, so callers
    /// that incorrectly treat the result as a full buffer observe garbage
    /// instead of silently-correct data. Callers that loop until the requested
    /// length is reached (or `Ok(0)` / EOF) keep working.
    pub fn short_read_probability(&mut self, value: f64) -> &mut Self {
        assert!(
            (0.0..=1.0).contains(&value),
            "short_read_probability must be between 0.0 and 1.0"
        );
        self.short_read_probability = value;
        self
    }

    /// Set whether to use noatime mount semantics.
    ///
    /// Default: true (atime not updated on reads).
    ///
    /// When true, reads leave the access time alone: `Metadata::accessed()`
    /// returns the file creation time until `File::set_times` sets another.
    /// This matches common Linux ext3/ext4 configurations.
    ///
    /// # Panics
    ///
    /// Setting this to `false` will panic. Access time tracking is not
    /// currently implemented.
    pub fn noatime(&mut self, value: bool) -> &mut Self {
        if !value {
            unimplemented!("atime tracking is not implemented; noatime must be true");
        }
        self.noatime = value;
        self
    }

    /// Set the required alignment in bytes for `O_DIRECT` I/O operations.
    ///
    /// Default: 512 bytes — the minimum alignment the Linux kernel enforces for
    /// `O_DIRECT`. Any read or write on a file opened with `direct_io(true)` will
    /// fail with `EINVAL` if the buffer pointer, file offset, or buffer length is
    /// not a multiple of this value.
    ///
    /// For realistic testing, set this to match your production device's logical
    /// block size. You can check it with:
    /// - NVMe: `nvme id-ns /dev/nvmeXnY` (look for "lbads" / LBA data size)
    /// - Block device: `blockdev --getbsz /dev/sdX`
    ///
    /// Common values: 512 (legacy), 4096 (modern NVMe/SSD).
    ///
    /// # Panics
    ///
    /// Panics if `bytes` is 0 or not a power of two.
    pub fn direct_io_alignment(&mut self, bytes: u64) -> &mut Self {
        assert!(bytes > 0, "direct_io_alignment must be positive");
        assert!(
            bytes.is_power_of_two(),
            "direct_io_alignment must be a power of two"
        );
        self.direct_io_alignment = bytes;
        self
    }

    /// Set the block size for torn write simulation.
    ///
    /// Default: None (writes are atomic).
    ///
    /// When set, pending writes may be partially applied on crash. This
    /// simulates real disk behavior where writes are only atomic within a
    /// single sector/block (typically 512 or 4096 bytes).
    ///
    /// On crash, each pending write is processed as follows:
    /// - The write is divided into blocks of the configured size
    /// - A random number of complete blocks (0 to all) survive the crash
    /// - Blocks are written sequentially from the start of the write
    ///
    /// # Example
    ///
    /// ```ignore
    /// // With block_size=4096, a 10KB write starting at offset 0 could survive as:
    /// // - Nothing written (0 blocks)
    /// // - First 4KB written (1 block)
    /// // - First 8KB written (2 blocks)
    /// // - All ~10KB written (3 blocks, last partial block included)
    ///
    /// let mut builder = Builder::new();
    /// builder.fs().block_size(4096);
    /// ```
    ///
    /// # Use Cases
    ///
    /// This is essential for testing:
    /// - Write-ahead logging (WAL) implementations
    /// - Database page writes
    /// - Any code that assumes atomic writes larger than a sector
    pub fn block_size(&mut self, bytes: u64) -> &mut Self {
        assert!(bytes > 0, "block_size must be positive");
        self.block_size = Some(bytes);
        self
    }

    /// Enable I/O latency simulation for async (tokio::fs) operations.
    ///
    /// Returns a mutable reference to the latency configuration, which is
    /// initialized with default values if not already set.
    ///
    /// Default latency: 50µs - 5ms with exponential distribution.
    ///
    /// # Note
    ///
    /// Only async operations (via `turmoil::fs::shim::tokio::fs`) incur latency.
    /// Sync operations (via `turmoil::fs::shim::std::fs`) complete immediately
    /// because there's no way to sleep in simulated time for sync code.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut builder = Builder::new();
    /// builder.fs().io_latency()
    ///     .min_latency(Duration::from_millis(1))
    ///     .max_latency(Duration::from_millis(50));
    /// ```
    pub fn io_latency(&mut self) -> &mut IoLatency {
        self.io_latency.get_or_insert_with(IoLatency::default)
    }

    /// Enable page cache simulation for async (tokio::fs) operations.
    ///
    /// Returns a mutable reference to the cache configuration, which is
    /// initialized with default values if not already set.
    ///
    /// When enabled, reads/writes track which pages are cached:
    /// - **Cache hits**: Near-instant (memory speed, ~100ns)
    /// - **Cache misses**: Full I/O latency (from `io_latency()` config)
    /// - **O_DIRECT**: Always bypasses cache (full latency)
    ///
    /// # Note
    ///
    /// Page cache only affects latency when `io_latency()` is also configured.
    /// Without I/O latency, all operations are instant regardless of cache state.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut builder = Builder::new();
    /// builder.fs().io_latency()
    ///     .min_latency(Duration::from_millis(1));
    /// builder.fs().page_cache()
    ///     .max_pages(1024)
    ///     .random_eviction_probability(0.1);  // 10% chaos
    /// ```
    pub fn page_cache(&mut self) -> &mut PageCacheConfig {
        self.page_cache.get_or_insert_with(PageCacheConfig::default)
    }
}

/// Cache key: (file, page index)
type PageKey = (Ino, u64);

/// LRU page cache with optional random eviction.
///
/// Tracks which (file, page) pairs are "hot" in memory. Cache hits
/// result in near-instant I/O, while cache misses incur full I/O latency.
/// Per-host page cache simulation. Tracks recently-touched offsets to
/// model cache hits in I/O latency.
pub struct PageCache {
    /// Pages currently in cache, ordered by recency (most recent at end)
    pages: IdSet<PageKey>,
    /// Configuration
    config: PageCacheConfig,
}

impl PageCache {
    /// Create a new page cache with the given configuration.
    pub fn new(config: PageCacheConfig) -> Self {
        Self {
            pages: IdSet::default(),
            config,
        }
    }

    /// Check if a page is cached and update LRU state.
    ///
    /// Returns `true` for cache hit, `false` for miss.
    /// Also handles random eviction if configured.
    pub fn access(&mut self, ino: Ino, offset: u64, rng: &mut dyn RngCore) -> bool {
        let key = (ino, offset / self.config.page_size);
        let hit = self.hit(ino, offset, rng);
        self.pages.shift_remove(&key);
        if hit {
            self.pages.insert(key);
        }
        hit
    }

    fn hit(&self, ino: Ino, offset: u64, rng: &mut dyn RngCore) -> bool {
        let key = (ino, offset / self.config.page_size);
        let p = self.config.random_eviction_probability;
        let evicted = p > 0.0 && rng.random_bool(p);
        !evicted && self.pages.contains(&key)
    }

    /// Insert a page into the cache.
    ///
    /// Called after read/write to populate cache. Evicts oldest pages
    /// if cache is at capacity.
    pub fn insert(&mut self, ino: Ino, offset: u64) {
        if self.config.max_pages == 0 {
            return;
        }
        let key = (ino, offset / self.config.page_size);
        if !self.pages.shift_remove(&key) && self.pages.len() == self.config.max_pages {
            self.pages.shift_remove_index(0);
        }
        self.pages.insert(key);
    }

    /// Invalidate all pages for a file.
    ///
    /// Called on truncate, which leaves the file's cached pages stale.
    pub fn invalidate_file(&mut self, ino: Ino) {
        self.pages.retain(|(file, _)| *file != ino);
    }

    /// Drops every page of `files`.
    fn forget(&mut self, files: &IdSet<Ino>) {
        if !files.is_empty() {
            self.pages.retain(|(file, _)| !files.contains(file));
        }
    }
}

/// An open file descriptor.
#[derive(Clone)]
pub struct OpenFile {
    /// The file the descriptor refers to, whatever its names are now.
    pub ino: Ino,
    /// Whether the file was opened with O_DIRECT semantics.
    pub direct_io: bool,
    /// Whether the descriptor allows reads.
    pub(crate) readable: bool,
    /// Whether the descriptor allows writes and truncation.
    pub(crate) writable: bool,
    /// Whether writes go to the end of the file, wherever they are aimed.
    pub(crate) append: bool,
    /// What a write makes durable before it returns.
    pub(crate) sync: WriteSync,
    /// Where a read or a write that is given no offset takes place.
    /// Descriptors duplicated from one another share it.
    pub(crate) cursor: Arc<AtomicU64>,
    /// The path the file was opened by.
    pub(crate) path: PathBuf,
}

/// Access a transfer needs from its file descriptor.
#[derive(Clone, Copy)]
pub enum Access {
    Read,
    Write,
}

/// What a write came to.
#[derive(Debug, Clone, Copy)]
pub struct Written {
    /// The bytes written.
    pub len: usize,
    /// Where in the file they went. For a descriptor that appends, that
    /// is the end the file had, wherever the write was aimed.
    pub offset: u64,
}

/// Whether a fault configured with `probability` strikes this time.
fn strikes(rng: &mut dyn RngCore, probability: f64) -> bool {
    probability > 0.0 && rng.random_bool(probability)
}

fn stale_handle() -> io::Error {
    io::Error::new(ErrorKind::NotFound, "file handle not found")
}

/// Per-host filesystem state.
///
/// Files, directories and symlinks are inodes in a tree that keeps a live
/// and a durable version of whatever syncs on its own:
///
/// - `sync()` makes a file's content and attributes durable.
/// - `sync_dir()` makes a directory's entries durable, as `sync()` does for
///   a directory that is open.
/// - `crash()` rolls everything back to its durable version and drops the
///   inodes no durable name reaches, such as a synced file whose directory
///   entry never was.
// TODO: narrow the public surface. turmoil-io-uring reads the probability
// knobs directly because its async completion path can't delegate to the
// synchronous shim.
pub struct FsState {
    /// Simulated time elapsed since unix epoch on this host. The
    /// embedder advances this before each tick (e.g. turmoil sets it
    /// to `host.timer.since_epoch()` before entering).
    pub now: Duration,

    /// Optional hook fired on every silent-corruption event. Set at
    /// build time or mutated on the locked `Fs`. Used by `turmoil`'s
    /// `unstable-barriers` integration.
    pub on_corruption: CorruptionHook,
    /// Per-host RNG, seeded from the world rng once at `Fs::new` time.
    /// Used for all probabilistic faults (sync_probability,
    /// io_error_probability, corruption_probability, short_read,
    /// torn writes, page-cache random eviction, latency sampling).
    ///
    /// Owning the rng here means fs faults don't shift the world rng
    /// state — adding a new probabilistic knob in one test no longer
    /// reorders network packet delivery in another. Replay determinism
    /// is preserved: same world seed → same per-host fs seed →
    /// same sequence.
    pub rng: Box<dyn RngCore + Send>,
    /// The namespace and the content of every file.
    pub(crate) tree: Tree,
    /// Open file descriptors.
    ///
    /// Allocated `RawFd`s start at [`SIM_FD_BASE`] so they can never collide
    /// with a real-OS fd if one accidentally leaks across the simulation
    /// boundary.
    open_files: IdMap<RawFd, OpenFile>,
    /// Next file descriptor to assign.
    next_fd: RawFd,
    /// Probability that writes are randomly synced to durable storage (0.0 - 1.0)
    pub sync_probability: f64,
    /// Disk capacity in bytes (None = unlimited)
    pub(crate) capacity: Option<u64>,
    /// Probability of I/O errors (0.0 - 1.0)
    pub io_error_probability: f64,
    /// Probability of silent data corruption on reads (0.0 - 1.0)
    pub corruption_probability: f64,
    /// Probability that a positional read returns a short count (0.0 - 1.0)
    pub short_read_probability: f64,
    /// Required alignment in bytes for O_DIRECT I/O
    pub direct_io_alignment: u64,
    /// Block size for torn write simulation (None = atomic writes)
    pub(crate) block_size: Option<u64>,
    /// I/O latency configuration (None = instant operations)
    pub(crate) io_latency: Option<IoLatency>,
    /// Page cache (None = no cache simulation)
    pub page_cache: Option<PageCache>,
}

impl FsState {
    /// Create a new empty filesystem state with a fresh per-host RNG
    /// seeded from `seed`. Embedders typically derive `seed` by calling
    /// `world_rng.next_u64()` at host construction time.
    pub fn new(config: FsConfig, seed: u64) -> Self {
        Self {
            now: Duration::ZERO,
            on_corruption: None,
            rng: Box::new(SmallRng::seed_from_u64(seed)),
            tree: Tree::new(config.capacity.is_some() && config.block_size.is_some()),
            open_files: IdMap::default(),
            next_fd: SIM_FD_BASE,
            sync_probability: config.sync_probability,
            capacity: config.capacity,
            io_error_probability: config.io_error_probability,
            corruption_probability: config.corruption_probability,
            short_read_probability: config.short_read_probability,
            direct_io_alignment: config.direct_io_alignment,
            block_size: config.block_size,
            io_latency: config.io_latency,
            page_cache: config.page_cache.map(PageCache::new),
        }
    }

    /// Calculate I/O latency for an operation.
    ///
    /// If `cache_hit` is true and page cache is enabled, returns near-instant
    /// latency (~100ns) simulating memory access speed.
    ///
    /// Returns `Duration::ZERO` if latency is not configured.
    pub fn calculate_latency(&mut self, cache_hit: bool) -> Duration {
        // Cache hits are near-instant (memory speed)
        if cache_hit && self.page_cache.is_some() {
            return Duration::from_nanos(100);
        }

        // Cache miss or no cache: full I/O latency
        match &self.io_latency {
            Some(cfg) => {
                let range = cfg.max_latency.saturating_sub(cfg.min_latency);
                let sample: f64 = cfg.distribution.sample(&mut self.rng);
                // Clamp sample to [0, 1] range for scaling
                let scaled = range.mul_f64(sample.min(1.0));
                cfg.min_latency + scaled
            }
            None => Duration::ZERO,
        }
    }

    /// Samples latency at submission. A cold page enters the cache when
    /// the read completes successfully.
    pub fn pending_read_latency(&mut self, fd: RawFd, offset: u64) -> Duration {
        let ino = self.buffered_inode(fd, Access::Read);
        let hit = match (ino, &self.page_cache) {
            (Some(ino), Some(cache)) => cache.hit(ino, offset, &mut *self.rng),
            _ => false,
        };
        self.calculate_latency(hit)
    }

    /// Records a successful buffered transfer at `offset`.
    pub fn cache_page(&mut self, fd: RawFd, access: Access, offset: u64) {
        let ino = self.buffered_inode(fd, access);
        if let (Some(ino), Some(cache)) = (ino, &mut self.page_cache) {
            cache.insert(ino, offset);
        }
    }

    fn buffered_inode(&self, fd: RawFd, access: Access) -> Option<Ino> {
        let file = self.file_for(fd, access).ok()?;
        (!file.direct_io).then_some(file.ino)
    }

    /// Latency and cache effects of a completed read.
    pub fn read_latency(&mut self, fd: RawFd, offset: u64) -> Duration {
        let latency = self.pending_read_latency(fd, offset);
        self.cache_page(fd, Access::Read, offset);
        latency
    }

    /// Latency and cache effects of a completed write.
    pub fn write_latency(&mut self, fd: RawFd, offset: u64) -> Duration {
        self.cache_page(fd, Access::Write, offset);
        self.calculate_latency(false)
    }

    /// Allocate a new sim-side file descriptor.
    ///
    /// Returns a `RawFd` from a high range (see [`SIM_FD_BASE`]) so that
    /// these integers cannot collide with real OS fds.
    ///
    /// `pub` (rather than `pub(crate)`) so sister crates that hold
    /// `&mut Fs` (notably `turmoil-io-uring`) can allocate fds — this
    /// is the published-but-unstable API, gated by `unstable-fs` on
    /// `turmoil`.
    pub fn alloc_fd(&mut self) -> RawFd {
        let fd = self.next_fd;
        self.next_fd = self
            .next_fd
            .checked_add(1)
            .expect("simulated fd counter overflowed RawFd range");
        fd
    }

    /// The open file behind `fd`.
    pub fn open_file(&self, fd: RawFd) -> Option<&OpenFile> {
        self.open_files.get(&fd)
    }

    /// The open file behind `fd`, which a handle on it takes to be there.
    pub(crate) fn descriptor(&self, fd: RawFd) -> io::Result<&OpenFile> {
        self.open_file(fd).ok_or_else(stale_handle)
    }

    /// Opens the file at `path` as `how` says, and returns a descriptor
    /// on it. Every open ends up here.
    pub(crate) fn open(&mut self, path: &Path, how: &Open) -> io::Result<RawFd> {
        let (ino, created) = self.tree.open(path, how, self.now)?;
        // A file that is new has nothing to truncate.
        if how.truncate && !created {
            self.resize(ino, 0)?;
        }
        let file = OpenFile {
            ino,
            direct_io: how.direct,
            readable: how.read,
            writable: how.write,
            append: how.append,
            sync: how.sync,
            cursor: Arc::default(),
            path: path.to_owned(),
        };
        Ok(self.install(file))
    }

    /// Gives `file` a descriptor.
    fn install(&mut self, file: OpenFile) -> RawFd {
        let fd = self.alloc_fd();
        self.tree.pin(file.ino);
        self.open_files.insert(fd, file);
        fd
    }

    /// Opens another descriptor on the file behind `fd`. The two share
    /// one cursor.
    pub(crate) fn dup(&mut self, fd: RawFd) -> io::Result<RawFd> {
        let file = self.descriptor(fd)?.clone();
        Ok(self.install(file))
    }

    pub(crate) fn close(&mut self, fd: RawFd) {
        if let Some(file) = self.open_files.swap_remove(&fd) {
            self.tree.unpin(file.ino);
            self.reclaim();
        }
    }

    /// Closes every descriptor, as the OS does for a process that exits.
    pub fn close_all(&mut self) {
        for (_, file) in self.open_files.drain(..) {
            self.tree.unpin(file.ino);
        }
        self.reclaim();
    }

    /// Drops the cached pages of files that can no longer be read.
    fn reclaim(&mut self) {
        let unreadable = self.tree.take_unreadable();
        if let Some(cache) = &mut self.page_cache {
            cache.forget(&unreadable);
        }
    }

    /// Resolves a descriptor before an operation uses its buffer or inode.
    /// As on Linux, one that is not open for the access is a bad descriptor.
    pub fn file_for(&self, fd: RawFd, access: Access) -> io::Result<&OpenFile> {
        let file = self.descriptor(fd)?;
        let open = match access {
            Access::Read => file.readable,
            Access::Write => file.writable,
        };
        open.then_some(file).ok_or(EBADF.into())
    }

    /// O_DIRECT I/O needs the buffer, the offset and the length aligned.
    fn check_alignment(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let alignment = self.direct_io_alignment;
        if (buf.as_ptr() as usize as u64).is_multiple_of(alignment)
            && offset.is_multiple_of(alignment)
            && (buf.len() as u64).is_multiple_of(alignment)
        {
            return Ok(());
        }
        Err(EINVAL.into())
    }

    /// Reads from the file behind `fd` at `offset`, subject to the faults
    /// the configuration injects. Every way of reading a file ends up here.
    pub fn read_at(&mut self, fd: RawFd, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let file = self.target(fd, Access::Read, offset, buf.len())?;
        let (ino, direct_io) = (file.ino, file.direct_io);
        if direct_io {
            self.check_alignment(buf, offset)?;
        }
        // A directory has no bytes, so the read fails before it gets as far
        // as the disk and its faults.
        let content = self.tree.file(ino)?;
        if strikes(&mut *self.rng, self.io_error_probability) {
            return Err(EIO.into());
        }
        let mut n = content.read(buf, offset);

        // Inject a short read: trim the reported count to something strictly
        // smaller than what was actually filled, and zero the tail so callers
        // that incorrectly assume full-read semantics observe garbage instead
        // of silently-correct data. We only do this when at least 2 bytes
        // were read, so the returned count is still > 0 (a return of 0 means
        // EOF, which is a distinct signal we don't want to fake). This
        // applies to O_DIRECT too — signals (EINTR) can short-circuit any
        // blocking syscall, including aligned O_DIRECT pread.
        if n > 1 && strikes(&mut *self.rng, self.short_read_probability) {
            let short = self.rng.random_range(1..n);
            buf[short..n].fill(0);
            n = short;
        }

        // Silent corruption flips at least one bit of one byte.
        if n > 0 && strikes(&mut *self.rng, self.corruption_probability) {
            let at = self.rng.random_range(0..n);
            buf[at] ^= self.rng.random::<u8>().max(1);
            self.corrupted(fd, offset + at as u64, 1);
        }

        Ok(n)
    }

    /// The open file that a transfer of `len` bytes at `offset` goes
    /// through. As on Linux, an offset is a signed number and so is the
    /// end of a transfer, and the descriptor is looked at in between.
    fn target(&self, fd: RawFd, access: Access, offset: u64, len: usize) -> io::Result<&OpenFile> {
        if offset > MAX_FILE_LEN {
            return Err(EINVAL.into());
        }
        let file = self.file_for(fd, access)?;
        match len as u64 > MAX_FILE_LEN - offset {
            true => Err(EINVAL.into()),
            false => Ok(file),
        }
    }

    /// Writes to the file behind `fd` at `offset`, subject to the faults
    /// the configuration injects. The write is pending until the file is
    /// synced. Every way of writing a file ends up here.
    pub fn write_at(&mut self, fd: RawFd, buf: &[u8], offset: u64) -> io::Result<Written> {
        let file = self.target(fd, Access::Write, offset, buf.len())?;
        let (ino, direct_io, sync) = (file.ino, file.direct_io, file.sync);
        // As on Linux, a descriptor opened to append writes at the end of
        // the file, wherever the write is aimed. A write of nothing goes
        // nowhere.
        let offset = match file.append && !buf.is_empty() {
            true => self.tree.stat(ino).len,
            false => offset,
        };
        if buf.len() as u64 > MAX_FILE_LEN - offset {
            return Err(EFBIG.into());
        }
        if direct_io {
            self.check_alignment(buf, offset)?;
        }
        if strikes(&mut *self.rng, self.io_error_probability) {
            return Err(EIO.into());
        }
        if let Some(capacity) = self.capacity {
            let growth = self.tree.growth(ino, offset, buf.len())?;
            if growth != 0 && growth > capacity.saturating_sub(self.tree.used_bytes()) {
                return Err(ENOSPC.into());
            }
        }

        if !buf.is_empty() {
            let change = Change::Write(offset, buf.to_vec());
            self.tree.change(ino, change, self.now)?;
        }
        self.settle(ino, sync);
        let len = buf.len();
        Ok(Written { len, offset })
    }

    fn cursor(&self, fd: RawFd) -> io::Result<&AtomicU64> {
        Ok(&self.descriptor(fd)?.cursor)
    }

    /// Reads at the cursor of the file behind `fd`, and moves the cursor
    /// past what was read.
    pub fn read(&mut self, fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
        let at = self.cursor(fd)?.load(Relaxed);
        let n = self.read_at(fd, buf, at)?;
        self.cursor(fd)?.store(at + n as u64, Relaxed);
        Ok(n)
    }

    /// Writes at the cursor of the file behind `fd`, and moves the cursor
    /// past what was written.
    pub fn write(&mut self, fd: RawFd, buf: &[u8]) -> io::Result<usize> {
        let at = self.cursor(fd)?.load(Relaxed);
        let written = self.write_at(fd, buf, at)?;
        let end = written.offset + written.len as u64;
        self.cursor(fd)?.store(end, Relaxed);
        Ok(written.len)
    }

    /// Moves the cursor of the file behind `fd`, and returns where it is.
    pub fn seek(&mut self, fd: RawFd, to: SeekFrom) -> io::Result<u64> {
        let file = self.descriptor(fd)?;
        let at = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.tree.stat(file.ino).len.checked_add_signed(offset),
            SeekFrom::Current(offset) => file.cursor.load(Relaxed).checked_add_signed(offset),
        };
        let at = at.filter(|&at| at <= MAX_FILE_LEN);
        let at = at.ok_or(EINVAL)?;
        file.cursor.store(at, Relaxed);
        Ok(at)
    }

    /// Truncates or zero-extends the file behind `fd`. Pending until the
    /// file is synced.
    pub(crate) fn set_len(&mut self, fd: RawFd, len: u64) -> io::Result<()> {
        let file = self.descriptor(fd)?;
        // To truncate through a descriptor that is not open for writing is
        // an invalid argument on Linux, rather than a bad descriptor.
        if len > MAX_FILE_LEN || !file.writable {
            return Err(EINVAL.into());
        }
        let (ino, sync) = (file.ino, file.sync);
        self.resize(ino, len)?;
        self.settle(ino, sync);
        Ok(())
    }

    /// Truncates or zero-extends a file. Pending until the file is synced.
    fn resize(&mut self, ino: Ino, len: u64) -> io::Result<()> {
        // Pages past the new length are stale. Dropping every page of the
        // file is simpler than finding them.
        if let Some(cache) = &mut self.page_cache {
            cache.invalidate_file(ino);
        }
        self.tree.change(ino, Change::SetLen(len), self.now)
    }

    /// What becomes of a change to a file on the disk right away. A
    /// descriptor that syncs what it writes makes it durable. Otherwise
    /// the OS or the disk may flush the file on its own: only its data
    /// goes, never the directory entry that names it.
    fn settle(&mut self, ino: Ino, sync: WriteSync) {
        match sync {
            WriteSync::Data => self.tree.sync_data(ino),
            WriteSync::All => self.tree.sync(ino),
            WriteSync::Nothing if strikes(&mut *self.rng, self.sync_probability) => {
                self.tree.sync(ino)
            }
            WriteSync::Nothing => {}
        }
    }

    /// Makes the content and attributes of the file behind `fd` durable,
    /// as fsync() does. The content of a directory is its entries.
    ///
    /// This does NOT make the file's directory entry durable. A new file
    /// survives a crash only once its parent directory is synced too.
    pub fn sync(&mut self, fd: RawFd) -> io::Result<()> {
        let ino = self.descriptor(fd)?.ino;
        self.tree.sync(ino);
        Ok(())
    }

    pub(crate) fn sync_data(&mut self, fd: RawFd) -> io::Result<()> {
        let ino = self.descriptor(fd)?.ino;
        self.tree.sync_data(ino);
        Ok(())
    }

    fn corrupted(&self, fd: RawFd, offset: u64, len: usize) {
        let (Some(hook), Some(file)) = (&self.on_corruption, self.open_files.get(&fd)) else {
            return;
        };
        let path = file.path.clone();
        let event = FsCorruption { path, offset, len };
        if CORRUPTIONS.is_set() {
            CORRUPTIONS.with(|events| events.borrow_mut().push((hook.clone(), event)));
        } else {
            hook(&event);
        }
    }

    /// Crash this host's filesystem.
    ///
    /// Everything pending is lost: unsynced writes, and names whose
    /// directory was not synced. A file whose data was synced but whose
    /// directory entry was not is gone, since nothing names it. Open file
    /// descriptors are closed.
    ///
    /// If `block_size` is configured, pending writes may be partially applied
    /// (torn writes) before being discarded. This simulates real disk behavior
    /// where writes spanning multiple blocks may be partially written on crash.
    ///
    /// The `sync_probability` config option handles random flushing during normal
    /// operation; by the time crash() is called, anything still pending is lost
    /// (except for torn write partial data).
    pub fn crash(&mut self) {
        self.open_files.clear();
        self.tree.crash(self.block_size, &mut *self.rng);
        // What the page cache held was in memory, and is gone with it.
        if let Some(cache) = &mut self.page_cache {
            cache.pages.clear();
        }
    }
}

impl Default for FsState {
    fn default() -> Self {
        Self::new(FsConfig::default(), 0)
    }
}

/// Builder for constructing an [`Fs`] with custom configuration.
///
/// ```ignore
/// let fs = Fs::builder()
///     .seed(42)
///     .sync_probability(0.1)
///     .capacity(1024 * 1024)
///     .build();
/// ```
#[derive(Default)]
pub struct FsBuilder {
    config: FsConfig,
    seed: u64,
    on_corruption: CorruptionHook,
}

impl FsBuilder {
    /// Replace the entire config at once. Individual builder methods
    /// (e.g. [`Self::sync_probability`]) override fields within this
    /// config, so call this first if combining both styles.
    pub fn config(mut self, config: FsConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the RNG seed (default: 0).
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Set the probability that writes are randomly synced (0.0–1.0).
    pub fn sync_probability(mut self, value: f64) -> Self {
        self.config.sync_probability(value);
        self
    }

    /// Set disk capacity in bytes per host.
    pub fn capacity(mut self, bytes: u64) -> Self {
        self.config.capacity(bytes);
        self
    }

    /// Set the probability of I/O errors (0.0–1.0).
    pub fn io_error_probability(mut self, value: f64) -> Self {
        self.config.io_error_probability(value);
        self
    }

    /// Set the probability of silent data corruption on reads (0.0–1.0).
    pub fn corruption_probability(mut self, value: f64) -> Self {
        self.config.corruption_probability(value);
        self
    }

    /// Set the probability of short reads (0.0–1.0).
    pub fn short_read_probability(mut self, value: f64) -> Self {
        self.config.short_read_probability(value);
        self
    }

    /// Set the alignment required for O_DIRECT I/O.
    pub fn direct_io_alignment(mut self, bytes: u64) -> Self {
        self.config.direct_io_alignment(bytes);
        self
    }

    /// Set the block size for torn-write simulation.
    pub fn block_size(mut self, bytes: u64) -> Self {
        self.config.block_size(bytes);
        self
    }

    /// Install a corruption hook fired on every silent-corruption event.
    pub fn on_corruption(mut self, hook: impl Fn(&FsCorruption) + Send + Sync + 'static) -> Self {
        self.on_corruption = Some(Arc::new(hook));
        self
    }

    /// Build the `Fs` from the configured values.
    pub fn build(self) -> Fs {
        let mut state = FsState::new(self.config, self.seed);
        state.on_corruption = self.on_corruption;
        Fs(Arc::new(Mutex::new(state)))
    }
}

/// A simulated filesystem handle.
///
/// `Fs` wraps the internal state behind an `Arc<Mutex<...>>`, making
/// it cheaply cloneable and sendable to worker threads. Construct via
/// [`Fs::builder`], then call [`Fs::enter`] to install it as the
/// current filesystem on the calling thread.
///
/// ```ignore
/// let fs = Fs::builder().seed(42).sync_probability(0.1).build();
/// let _guard = fs.enter();
/// // shim operations now route through this Fs
/// ```
///
/// For worker threads, clone the handle and call `enter` on the clone:
///
/// ```ignore
/// let fs2 = fs.clone();
/// std::thread::spawn(move || {
///     let _guard = fs2.enter();
///     // filesystem operations work here
/// });
/// ```
#[derive(Clone)]
pub struct Fs(Arc<Mutex<FsState>>);

impl fmt::Debug for Fs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fs").finish_non_exhaustive()
    }
}

impl Fs {
    /// Returns a builder for constructing an `Fs` with custom
    /// configuration.
    pub fn builder() -> FsBuilder {
        FsBuilder::default()
    }

    /// Clone the current host's filesystem.
    ///
    /// Use this from an entered simulation context to move the current
    /// filesystem into a worker thread. The worker must call
    /// [`Fs::enter`] before using filesystem shims.
    ///
    /// # Panics
    ///
    /// Panics if called without an entered `Fs` on this thread.
    pub fn current() -> Self {
        Self::try_current().expect("turmoil-fs: no Fs is current (call Fs::enter first)")
    }

    fn try_current() -> Option<Self> {
        CURRENT_FS_ARC.with_borrow(Clone::clone).map(Self)
    }

    /// Install this `Fs` as the current filesystem on the calling
    /// thread. While the guard is held, shim operations route through
    /// this `Fs`.
    pub fn enter(&self) -> FsEnterGuard {
        enter(&self.0)
    }

    /// Runs `f` with this filesystem's state locked.
    pub(crate) fn context<R>(&self, f: impl FnOnce(FsContext<'_>) -> R) -> R {
        let locked = || {
            let mut fs = self.0.lock().expect("FsState mutex poisoned");
            let now = fs.now;
            let result = f(FsContext { fs: &mut fs, now });
            fs.reclaim();
            result
        };
        if CORRUPTIONS.is_set() {
            return locked();
        }
        let events = RefCell::new(Vec::new());
        let result = CORRUPTIONS.set(&events, locked);
        for (hook, event) in events.into_inner() {
            hook(&event);
        }
        result
    }

    /// Closes every descriptor on this filesystem, as the OS does when the
    /// process holding them exits. The embedder calls this when it restarts
    /// a host's software.
    pub fn close_all(&self) {
        self.0.lock().expect("FsState mutex poisoned").close_all();
    }

    /// Advance simulated time. The embedder calls this before each
    /// tick so that timestamps on newly created files reflect the
    /// simulation clock.
    pub fn set_now(&self, now: Duration) {
        self.0.lock().expect("FsState mutex poisoned").now = now;
    }

    /// Crash this host's filesystem, discarding pending (unsynced)
    /// writes and orphaning files whose directory entries were never
    /// synced. See [`FsState::crash`] for the full semantics.
    pub fn crash(&self) {
        self.0.lock().expect("FsState mutex poisoned").crash();
    }
}

/// A descriptor can close itself on its original filesystem without
/// keeping that filesystem alive after the host exits.
#[derive(Clone, Debug)]
struct FileOwner(Weak<Mutex<FsState>>);

impl FileOwner {
    fn new(fs: &Fs) -> Self {
        Self(Arc::downgrade(&fs.0))
    }

    fn entered(&self) -> io::Result<Fs> {
        let fs = Fs::current();
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&fs.0))
            .then_some(fs)
            .ok_or_else(stale_handle)
    }

    fn close(&self, fd: RawFd) {
        if let Some(fs) = self.0.upgrade() {
            if let Ok(mut state) = fs.lock() {
                state.close(fd);
            }
        }
    }
}

impl Default for Fs {
    fn default() -> Self {
        Fs(Arc::new(Mutex::new(FsState::default())))
    }
}
