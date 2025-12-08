//! Simulated filesystem for turmoil.
//!
//! This module provides a drop-in replacement for `std::fs` and `std::os::unix::fs`
//! that simulates filesystem behavior with durability testing support.
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
//! - File creation and writes go to a "pending" state
//! - Pending operations become durable on `sync_all()` or randomly based on [`FsConfig::sync_probability`]
//! - On crash (`sim.crash()`):
//!   - Files that were never synced are deleted entirely
//!   - Pending writes to synced files are lost; durable data survives
//! - Each host has its own isolated filesystem namespace

pub mod shim;

use crate::world::World;
use indexmap::{IndexMap, IndexSet};
use rand::{Rng, RngCore};
use rand_distr::{Distribution, Exp};
use std::cell::RefCell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Thread-local storage for worker thread filesystem context.
// When a worker thread calls `FsHandle::enter()`, this stores the context
// so that `FsContext::current()` can find it.
thread_local! {
    static WORKER_FS_CONTEXT: RefCell<Option<WorkerContext>> = const { RefCell::new(None) };
}

/// Context stored in thread-local for worker threads.
struct WorkerContext {
    /// Shared reference to the filesystem
    fs: Arc<Mutex<Fs>>,
    /// Captured simulated time
    time: Duration,
    /// RNG for this worker thread (uses its own state for thread safety)
    rng: Mutex<rand::rngs::ThreadRng>,
}

/// A handle to the current host's filesystem that can be sent to worker threads.
///
/// Use this to perform filesystem operations from threads spawned outside
/// the turmoil simulation context (e.g., via `std::thread::spawn` or
/// `tokio::task::spawn_blocking`).
///
/// # Example
///
/// ```ignore
/// use turmoil::fs::FsHandle;
/// use turmoil::fs::shim::std::fs::{create_dir, write, read};
///
/// // In turmoil simulation:
/// create_dir("/data")?;
///
/// // Capture handle to current host's filesystem
/// let handle = FsHandle::current();
///
/// // Spawn worker thread
/// let worker = std::thread::spawn(move || {
///     // Enter the filesystem context
///     let _guard = handle.enter();
///
///     // Now filesystem operations work
///     write("/data/from_worker.txt", b"hello")?;
///     Ok::<_, std::io::Error>(())
/// });
///
/// worker.join().unwrap()?;
///
/// // Data is visible from main thread
/// assert_eq!(read("/data/from_worker.txt")?, b"hello");
/// ```
///
/// # Thread Safety
///
/// The handle holds an `Arc<Mutex<Fs>>`, so it is safe to send to other threads.
/// The mutex ensures exclusive access to the filesystem during operations.
/// However, users should be aware that concurrent access from multiple threads
/// may lead to non-deterministic behavior in tests.
pub struct FsHandle {
    /// Shared reference to the host's filesystem
    fs: Arc<Mutex<Fs>>,
    /// Simulated time when the handle was captured
    time: Duration,
}

impl FsHandle {
    /// Capture a handle to the current host's filesystem.
    ///
    /// Must be called from within a turmoil simulation context.
    ///
    /// # Panics
    ///
    /// Panics if called outside a turmoil simulation.
    pub fn current() -> Self {
        World::current(|world| {
            let addr = world.current.expect("current host missing");
            let host = world.hosts.get(&addr).unwrap();
            let time = host.timer.since_epoch();

            FsHandle {
                fs: Arc::clone(&host.fs),
                time,
            }
        })
    }

    /// Enter the filesystem context for this thread.
    ///
    /// Returns a guard that must be held while performing filesystem operations.
    /// When the guard is dropped, the context is cleared.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handle = FsHandle::current();
    /// std::thread::spawn(move || {
    ///     let _guard = handle.enter();
    ///     // filesystem operations work here
    /// });
    /// ```
    pub fn enter(&self) -> FsHandleGuard {
        WORKER_FS_CONTEXT.with(|ctx| {
            *ctx.borrow_mut() = Some(WorkerContext {
                fs: Arc::clone(&self.fs),
                time: self.time,
                rng: Mutex::new(rand::rng()),
            });
        });
        FsHandleGuard { _private: () }
    }
}

/// Guard that maintains the filesystem context for a worker thread.
///
/// When dropped, clears the thread-local context.
#[must_use = "the filesystem context is only active while this guard is held"]
pub struct FsHandleGuard {
    /// Prevent external construction for forward compatibility.
    _private: (),
}

impl Drop for FsHandleGuard {
    fn drop(&mut self) {
        WORKER_FS_CONTEXT.with(|ctx| {
            *ctx.borrow_mut() = None;
        });
    }
}

/// Context for filesystem operations, bundling Fs state with RNG and time.
///
/// This abstracts how the filesystem is accessed, allowing future support
/// for worker threads that aren't running in the main simulation context.
pub(crate) struct FsContext<'a> {
    /// The filesystem state
    pub fs: &'a mut Fs,
    /// Random number generator for probabilistic behaviors
    rng: &'a mut dyn RngCore,
    /// Current simulated time
    pub now: Duration,
}

impl FsContext<'_> {
    /// Check if we're in a worker thread context.
    fn in_worker_context() -> bool {
        WORKER_FS_CONTEXT.with(|ctx| ctx.borrow().is_some())
    }

    /// Run `f` with the current host's filesystem context.
    ///
    /// This checks two sources for the context:
    /// 1. Thread-local storage (for worker threads using [`FsHandle::enter`])
    /// 2. The turmoil World (for the main simulation thread)
    ///
    /// # Panics
    ///
    /// Panics if called outside both a turmoil simulation and a worker thread context.
    pub fn current<R>(f: impl FnOnce(FsContext<'_>) -> R) -> R {
        // Check which context we're in first, then call the appropriate path
        if Self::in_worker_context() {
            WORKER_FS_CONTEXT.with(|ctx| {
                let borrowed = ctx.borrow();
                let worker_ctx = borrowed.as_ref().unwrap();

                // Lock the filesystem mutex
                let mut fs_guard = worker_ctx.fs.lock().unwrap();
                let now = worker_ctx.time;

                // Get mutable access to the RNG
                let mut rng_guard = worker_ctx.rng.lock().unwrap();

                f(FsContext {
                    fs: &mut fs_guard,
                    rng: &mut rng_guard,
                    now,
                })
            })
        } else {
            // Fall back to World::current for main simulation thread
            World::current(|world| {
                let addr = world.current.expect("current host missing");
                let now = world.hosts[&addr].timer.since_epoch();

                // Destructure world to get disjoint mutable borrows of hosts and rng
                let World { hosts, rng, .. } = world;
                let host = hosts.get_mut(&addr).unwrap();

                // Lock the filesystem mutex
                let mut fs_guard = host.fs.lock().unwrap();

                f(FsContext {
                    fs: &mut fs_guard,
                    rng: &mut **rng,
                    now,
                })
            })
        }
    }

    /// Run `f` if we're in a simulation context, otherwise no-op.
    ///
    /// Used in drop paths where the simulation may be shutting down.
    /// Checks both worker thread context and World context.
    pub fn current_if_set(f: impl FnOnce(FsContext<'_>)) {
        // Check which context we're in first
        if Self::in_worker_context() {
            WORKER_FS_CONTEXT.with(|ctx| {
                let borrowed = ctx.borrow();
                let worker_ctx = borrowed.as_ref().unwrap();

                // Lock the filesystem mutex
                let mut fs_guard = worker_ctx.fs.lock().unwrap();
                let now = worker_ctx.time;
                let mut rng_guard = worker_ctx.rng.lock().unwrap();

                f(FsContext {
                    fs: &mut fs_guard,
                    rng: &mut rng_guard,
                    now,
                });
            });
        } else {
            // Fall back to World::current_if_set
            World::current_if_set(|world| {
                let addr = world.current.expect("current host missing");
                let now = world.hosts[&addr].timer.since_epoch();

                let World { hosts, rng, .. } = world;
                let host = hosts.get_mut(&addr).unwrap();

                // Lock the filesystem mutex
                let mut fs_guard = host.fs.lock().unwrap();

                f(FsContext {
                    fs: &mut fs_guard,
                    rng: &mut **rng,
                    now,
                })
            })
        }
    }

    /// Returns true with the given probability (0.0 to 1.0).
    pub fn random_bool(&mut self, probability: f64) -> bool {
        self.rng.random_bool(probability)
    }

    /// Returns a random value in the given range.
    pub fn random_range(&mut self, range: Range<usize>) -> usize {
        self.rng.random_range(range)
    }

    /// Returns a random u8 value.
    pub fn random_u8(&mut self) -> u8 {
        self.rng.random()
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
    /// Path of the file that was corrupted
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
/// - `noatime`: true (access time not updated on reads)
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
    /// Whether to use noatime semantics (atime not updated on reads)
    pub(crate) noatime: bool,
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
            noatime: true,
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

    /// Set whether to use noatime mount semantics.
    ///
    /// Default: true (atime not updated on reads).
    ///
    /// When true, `Metadata::accessed()` returns the file creation time
    /// rather than tracking access times on every read. This matches
    /// common Linux ext3/ext4 configurations.
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

/// A pending filesystem operation that hasn't been synced to durable storage.
#[derive(Debug, Clone)]
pub(crate) enum PendingOp {
    /// Create a file (durable when parent dir synced)
    CreateFile {
        path: PathBuf,
        /// Timestamp when file was created
        time: Duration,
        /// Unix permission bits
        mode: u32,
    },
    /// Create a directory (durable when parent dir synced)
    CreateDir {
        path: PathBuf,
        /// Timestamp when directory was created
        time: Duration,
        /// Unix permission bits
        mode: u32,
    },
    /// Create a symlink (durable when parent dir synced)
    CreateSymlink {
        path: PathBuf,
        target: PathBuf,
        time: Duration,
    },
    /// Create a hard link (durable when parent dir synced)
    CreateHardLink {
        path: PathBuf,
        target: PathBuf,
        time: Duration,
    },
    /// Write data to a file (durable when file synced)
    Write {
        path: PathBuf,
        offset: u64,
        data: Vec<u8>,
        /// Timestamp when write occurred
        time: Duration,
    },
    /// Truncate/extend a file (durable when file synced)
    SetLen {
        path: PathBuf,
        len: u64,
        /// Timestamp when truncate/extend occurred
        time: Duration,
    },
    /// Change file permissions
    SetPermissions {
        path: PathBuf,
        mode: u32,
        time: Duration,
    },
    /// Rename a file or directory (durable when parent dir synced)
    Rename { from: PathBuf, to: PathBuf },
    /// Remove a file (durable when parent dir synced)
    RemoveFile { path: PathBuf },
    /// Remove a directory (durable when parent dir synced)
    RemoveDir { path: PathBuf },
}

/// Stored file data including content and timestamps.
///
/// Timestamps use turmoil's simulated time (duration since epoch).
/// We model noatime behavior: atime is not tracked (returns crtime).
#[derive(Debug, Clone)]
pub(crate) struct FileData {
    /// File content
    pub(crate) content: Vec<u8>,
    /// Creation time (set once, never changes)
    pub(crate) crtime: Duration,
    /// Modification time (updated on writes)
    pub(crate) mtime: Duration,
    /// Change time (updated on any metadata change, including writes)
    pub(crate) ctime: Duration,
    /// Unix permission bits (default: 0o644)
    pub(crate) mode: u32,
    /// Number of hard links to this file
    pub(crate) nlink: u64,
}

impl FileData {
    /// Create new empty file data with all timestamps set to the given time.
    #[allow(dead_code)]
    fn new(now: Duration) -> Self {
        Self::with_mode(now, 0o644)
    }

    /// Create new empty file data with specified mode.
    fn with_mode(now: Duration, mode: u32) -> Self {
        Self {
            content: Vec::new(),
            crtime: now,
            mtime: now,
            ctime: now,
            mode,
            nlink: 1,
        }
    }
}

/// Stored directory data including timestamps.
#[derive(Debug, Clone)]
pub(crate) struct DirData {
    /// Creation time (set once, never changes)
    pub(crate) crtime: Duration,
    /// Modification time (updated when contents change)
    pub(crate) mtime: Duration,
    /// Change time (updated on any metadata change)
    pub(crate) ctime: Duration,
    /// Unix permission bits (default: 0o755)
    pub(crate) mode: u32,
}

impl DirData {
    /// Create new directory data with all timestamps set to the given time.
    fn new(now: Duration) -> Self {
        Self::with_mode(now, 0o755)
    }

    /// Create new directory data with specified mode.
    fn with_mode(now: Duration, mode: u32) -> Self {
        Self {
            crtime: now,
            mtime: now,
            ctime: now,
            mode,
        }
    }
}

/// Stored symlink data.
#[derive(Debug, Clone)]
pub(crate) struct SymlinkData {
    /// Target path the symlink points to
    pub(crate) target: PathBuf,
    /// Creation time
    pub(crate) crtime: Duration,
    /// Modification time
    pub(crate) mtime: Duration,
    /// Change time
    pub(crate) ctime: Duration,
}

impl SymlinkData {
    fn new(target: PathBuf, now: Duration) -> Self {
        Self {
            target,
            crtime: now,
            mtime: now,
            ctime: now,
        }
    }
}

/// Cache key: (file path, page index)
type PageKey = (PathBuf, u64);

/// LRU page cache with optional random eviction.
///
/// Tracks which (file, page) pairs are "hot" in memory. Cache hits
/// result in near-instant I/O, while cache misses incur full I/O latency.
pub(crate) struct PageCache {
    /// Pages currently in cache, ordered by recency (most recent at end)
    pages: IndexSet<PageKey>,
    /// Configuration
    config: PageCacheConfig,
}

impl PageCache {
    /// Create a new page cache with the given configuration.
    pub fn new(config: PageCacheConfig) -> Self {
        Self {
            pages: IndexSet::new(),
            config,
        }
    }

    /// Check if a page is cached and update LRU state.
    ///
    /// Returns `true` for cache hit, `false` for miss.
    /// Also handles random eviction if configured.
    pub fn access(&mut self, path: &Path, offset: u64, rng: &mut dyn RngCore) -> bool {
        let page_idx = offset / self.config.page_size;
        let key = (path.to_path_buf(), page_idx);

        // Random eviction chaos - evict this page if it exists
        if self.config.random_eviction_probability > 0.0
            && rng.random_bool(self.config.random_eviction_probability)
        {
            self.pages.swap_remove(&key);
            return false;
        }

        // Check if cached (and promote to most recent)
        if self.pages.swap_remove(&key) {
            self.pages.insert(key);
            return true;
        }

        false
    }

    /// Insert a page into the cache.
    ///
    /// Called after read/write to populate cache. Evicts oldest pages
    /// if cache is at capacity.
    pub fn insert(&mut self, path: &Path, offset: u64) {
        let page_idx = offset / self.config.page_size;
        let key = (path.to_path_buf(), page_idx);

        // Evict oldest if at capacity
        while self.pages.len() >= self.config.max_pages {
            self.pages.shift_remove_index(0);
        }

        // Remove if exists (will re-add at end for LRU)
        self.pages.swap_remove(&key);
        self.pages.insert(key);
    }

    /// Invalidate all pages for a file.
    ///
    /// Called on truncate, delete, or other operations that
    /// invalidate cached data.
    pub fn invalidate_file(&mut self, path: &Path) {
        self.pages.retain(|(p, _)| p != path);
    }
}

/// Per-host filesystem state.
///
/// Models POSIX filesystem semantics with separate inode data and directory entries:
///
/// - `persisted_*` maps: Hold inode data (file contents, directory metadata, symlink targets).
///   This data survives crashes once written.
/// - `synced_entries`: Tracks which paths have durable directory entries.
///   Without a synced entry, data exists but is unreachable (orphaned).
/// - `pending`: Queue of operations not yet synced to durable storage.
///
/// Syncing works in two stages:
/// - `sync_file()`: Makes file data durable (writes to `persisted_files`)
/// - `sync_dir()`: Makes directory entries durable (adds paths to `synced_entries`)
///
/// On crash, orphaned files (in `persisted_files` but not `synced_entries`) are removed
/// since they have no directory entry pointing to them.
pub(crate) struct Fs {
    /// File inode data (content, timestamps, mode). Data here survives crash,
    /// but is only reachable if the path is also in `synced_entries`.
    pub(crate) persisted_files: IndexMap<PathBuf, FileData>,
    /// Directory inode data. Same reachability rules as files.
    pub(crate) persisted_dirs: IndexMap<PathBuf, DirData>,
    /// Symlink inode data. Same reachability rules as files.
    pub(crate) persisted_symlinks: IndexMap<PathBuf, SymlinkData>,
    /// Paths with durable directory entries. On crash, only paths in this set
    /// remain accessible; others are orphaned and cleaned up.
    synced_entries: IndexSet<PathBuf>,
    /// Queue of pending operations (lost on crash unless synced)
    pub(crate) pending: Vec<PendingOp>,
    /// Open file handles: fd -> path
    pub(crate) open_handles: IndexMap<u64, PathBuf>,
    /// Next file descriptor to assign
    next_fd: u64,
    /// Probability that writes are randomly synced to durable storage (0.0 - 1.0)
    pub(crate) sync_probability: f64,
    /// Disk capacity in bytes (None = unlimited)
    pub(crate) capacity: Option<u64>,
    /// Probability of I/O errors (0.0 - 1.0)
    pub(crate) io_error_probability: f64,
    /// Probability of silent data corruption on reads (0.0 - 1.0)
    pub(crate) corruption_probability: f64,
    /// Block size for torn write simulation (None = atomic writes)
    pub(crate) block_size: Option<u64>,
    /// I/O latency configuration (None = instant operations)
    pub(crate) io_latency: Option<IoLatency>,
    /// Page cache (None = no cache simulation)
    pub(crate) page_cache: Option<PageCache>,
}

impl Fs {
    /// Create a new empty filesystem.
    pub(crate) fn new(config: FsConfig) -> Self {
        let mut persisted_dirs = IndexMap::new();
        let mut synced_entries = IndexSet::new();
        // Root directory exists from the start with epoch timestamp
        persisted_dirs.insert(PathBuf::from("/"), DirData::new(Duration::ZERO));
        synced_entries.insert(PathBuf::from("/"));

        Self {
            persisted_files: IndexMap::new(),
            persisted_dirs,
            persisted_symlinks: IndexMap::new(),
            synced_entries,
            pending: Vec::new(),
            open_handles: IndexMap::new(),
            next_fd: 0,
            sync_probability: config.sync_probability,
            capacity: config.capacity,
            io_error_probability: config.io_error_probability,
            corruption_probability: config.corruption_probability,
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
    pub(crate) fn calculate_latency(&self, rng: &mut dyn RngCore, cache_hit: bool) -> Duration {
        // Cache hits are near-instant (memory speed)
        if cache_hit && self.page_cache.is_some() {
            return Duration::from_nanos(100);
        }

        // Cache miss or no cache: full I/O latency
        match &self.io_latency {
            Some(cfg) => {
                let range = cfg.max_latency.saturating_sub(cfg.min_latency);
                let sample: f64 = cfg.distribution.sample(rng);
                // Clamp sample to [0, 1] range for scaling
                let scaled = range.mul_f64(sample.min(1.0));
                cfg.min_latency + scaled
            }
            None => Duration::ZERO,
        }
    }

    /// Calculate total bytes used (persisted + pending writes).
    pub(crate) fn used_bytes(&self) -> u64 {
        // Start with persisted file sizes
        let mut total: u64 = self
            .persisted_files
            .values()
            .map(|v| v.content.len() as u64)
            .sum();

        // Account for pending operations that change size
        for op in &self.pending {
            match op {
                PendingOp::Write {
                    path, offset, data, ..
                } => {
                    let persisted_len = self
                        .persisted_files
                        .get(path)
                        .map(|v| v.content.len() as u64)
                        .unwrap_or(0);
                    let write_end = offset + data.len() as u64;
                    if write_end > persisted_len {
                        total += write_end - persisted_len;
                    }
                }
                PendingOp::SetLen { path, len, .. } => {
                    let persisted_len = self
                        .persisted_files
                        .get(path)
                        .map(|v| v.content.len() as u64)
                        .unwrap_or(0);
                    if *len > persisted_len {
                        total += len - persisted_len;
                    }
                }
                _ => {}
            }
        }
        total
    }

    /// Check if writing `additional_bytes` would exceed capacity.
    /// Returns Ok(()) if there's space, Err with ENOSPC message otherwise.
    pub(crate) fn check_space(&self, additional_bytes: u64) -> Result<(), &'static str> {
        if let Some(cap) = self.capacity {
            let used = self.used_bytes();
            if used.saturating_add(additional_bytes) > cap {
                return Err("No space left on device");
            }
        }
        Ok(())
    }

    /// Allocate a new file descriptor.
    pub(crate) fn alloc_fd(&mut self) -> u64 {
        let fd = self.next_fd;
        self.next_fd += 1;
        fd
    }

    /// Discard pending operations (called on crash).
    ///
    /// If `block_size` is configured, pending writes may be partially applied
    /// (torn writes) before being discarded. This simulates real disk behavior
    /// where writes spanning multiple blocks may be partially written on crash.
    ///
    /// The `sync_probability` config option handles random flushing during normal
    /// operation; by the time crash() is called, anything still pending is lost
    /// (except for torn write partial data).
    ///
    /// This also removes "orphaned" files - files whose data was synced (via
    /// `sync_file`) but whose directory entry was not synced (via `sync_dir`).
    /// In POSIX, both are required for a file to survive a crash.
    pub(crate) fn discard_pending(&mut self, rng: &mut dyn RngCore) {
        // Apply torn writes if block_size is configured
        if let Some(block_size) = self.block_size {
            self.apply_torn_writes(block_size, rng);
        }

        // Clear all pending operations - they're lost on crash
        self.pending.clear();

        // Remove orphaned files/dirs/symlinks (those whose directory entries weren't synced)
        // This models POSIX behavior where fsync(file) makes inode durable but
        // fsync(dir) is needed to make the directory entry durable.
        self.persisted_files
            .retain(|path, _| self.synced_entries.contains(path));
        self.persisted_dirs
            .retain(|path, _| self.synced_entries.contains(path));
        self.persisted_symlinks
            .retain(|path, _| self.synced_entries.contains(path));
    }

    /// Apply torn writes for pending Write operations.
    ///
    /// For each pending write, randomly decide how many blocks survive and
    /// apply the partial write to persisted state.
    fn apply_torn_writes(&mut self, block_size: u64, rng: &mut dyn RngCore) {
        // Collect writes to apply (we need to separate iteration from mutation)
        let torn_writes: Vec<_> = self
            .pending
            .iter()
            .filter_map(|op| {
                if let PendingOp::Write {
                    path,
                    offset,
                    data,
                    time,
                } = op
                {
                    // Only process writes to files that exist and have synced directory entries
                    // (otherwise the file would be orphaned anyway)
                    if !self.synced_entries.contains(path) {
                        return None;
                    }

                    // Calculate how many blocks this write spans
                    let total_blocks = (data.len() as u64).div_ceil(block_size);
                    if total_blocks == 0 {
                        return None;
                    }

                    // Randomly decide how many blocks survive (0 to total_blocks inclusive)
                    let surviving_blocks = rng.random_range(0..=total_blocks as usize) as u64;
                    if surviving_blocks == 0 {
                        return None;
                    }

                    // Calculate how many bytes survive
                    let surviving_bytes =
                        (surviving_blocks * block_size).min(data.len() as u64) as usize;

                    Some((
                        path.clone(),
                        *offset,
                        data[..surviving_bytes].to_vec(),
                        *time,
                    ))
                } else {
                    None
                }
            })
            .collect();

        // Apply the torn writes to persisted state
        for (path, offset, data, time) in torn_writes {
            if let Some(file_data) = self.persisted_files.get_mut(&path) {
                let end = offset as usize + data.len();
                if end > file_data.content.len() {
                    file_data.content.resize(end, 0);
                }
                file_data.content[offset as usize..end].copy_from_slice(&data);
                file_data.mtime = time;
                file_data.ctime = time;
            }
        }
    }

    /// Apply a single operation to persisted state.
    fn apply_op_to_persisted(&mut self, op: &PendingOp) {
        match op {
            PendingOp::CreateFile { path, time, mode } => {
                self.persisted_files
                    .entry(path.clone())
                    .or_insert_with(|| FileData::with_mode(*time, *mode));
            }
            PendingOp::CreateDir { path, time, mode } => {
                self.persisted_dirs
                    .entry(path.clone())
                    .or_insert_with(|| DirData::with_mode(*time, *mode));
            }
            PendingOp::CreateSymlink { path, target, time } => {
                self.persisted_symlinks
                    .entry(path.clone())
                    .or_insert_with(|| SymlinkData::new(target.clone(), *time));
            }
            PendingOp::CreateHardLink { path, target, time } => {
                // Hard link: create a new file entry pointing to the same content
                // In our simple model, we copy the file data and increment nlink on both
                if let Some(target_data) = self.persisted_files.get(target).cloned() {
                    let new_nlink = target_data.nlink + 1;
                    // Increment nlink on the target
                    if let Some(t) = self.persisted_files.get_mut(target) {
                        t.nlink = new_nlink;
                        t.ctime = *time; // ctime updates when nlink changes
                    }
                    // Create new entry with same content and same nlink
                    let mut link_data = target_data;
                    link_data.nlink = new_nlink;
                    self.persisted_files.insert(path.clone(), link_data);
                }
            }
            PendingOp::Write {
                path,
                offset,
                data,
                time,
            } => {
                if let Some(file_data) = self.persisted_files.get_mut(path) {
                    let end = *offset as usize + data.len();
                    if end > file_data.content.len() {
                        file_data.content.resize(end, 0);
                    }
                    file_data.content[*offset as usize..end].copy_from_slice(data);
                    // Update mtime and ctime
                    file_data.mtime = *time;
                    file_data.ctime = *time;
                }
            }
            PendingOp::SetLen { path, len, time } => {
                if let Some(file_data) = self.persisted_files.get_mut(path) {
                    file_data.content.resize(*len as usize, 0);
                    // Update mtime and ctime
                    file_data.mtime = *time;
                    file_data.ctime = *time;
                }
            }
            PendingOp::SetPermissions { path, mode, time } => {
                if let Some(file_data) = self.persisted_files.get_mut(path) {
                    file_data.mode = *mode;
                    file_data.ctime = *time;
                } else if let Some(dir_data) = self.persisted_dirs.get_mut(path) {
                    dir_data.mode = *mode;
                    dir_data.ctime = *time;
                }
            }
            PendingOp::Rename { from, to } => {
                if let Some(file_data) = self.persisted_files.swap_remove(from) {
                    self.persisted_files.insert(to.clone(), file_data);
                } else if let Some(dir_data) = self.persisted_dirs.swap_remove(from) {
                    self.persisted_dirs.insert(to.clone(), dir_data);
                } else if let Some(symlink_data) = self.persisted_symlinks.swap_remove(from) {
                    self.persisted_symlinks.insert(to.clone(), symlink_data);
                }
            }
            PendingOp::RemoveFile { path } => {
                // Remove from files or symlinks (unlink works on both)
                self.persisted_files.swap_remove(path);
                self.persisted_symlinks.swap_remove(path);
            }
            PendingOp::RemoveDir { path } => {
                self.persisted_dirs.swap_remove(path);
            }
        }
    }

    /// Resolve the effective persisted path after accounting for pending renames.
    /// Returns the path where data is currently stored in persisted state.
    fn resolve_persisted_path(&self, path: &Path) -> PathBuf {
        let mut current = path.to_path_buf();
        // Walk backwards through renames to find the original persisted path
        for op in self.pending.iter().rev() {
            if let PendingOp::Rename { from, to } = op {
                if to == &current {
                    current = from.clone();
                }
            }
        }
        current
    }

    /// Resolve hard link target if path is a pending hard link.
    /// Returns the target path if this is a hard link, otherwise returns None.
    fn resolve_hardlink_target(&self, path: &Path) -> Option<PathBuf> {
        for op in &self.pending {
            if let PendingOp::CreateHardLink {
                path: p, target, ..
            } = op
            {
                if p == path {
                    return Some(target.clone());
                }
            }
        }
        None
    }

    /// Resolve the effective content path for a file (handles hard links and renames).
    fn resolve_content_path(&self, path: &Path) -> PathBuf {
        // First check if this is a hard link
        if let Some(target) = self.resolve_hardlink_target(path) {
            // Recursively resolve in case the target is also a link or renamed
            return self.resolve_content_path(&target);
        }
        // Otherwise resolve renames
        self.resolve_persisted_path(path)
    }

    /// Check if a file exists (persisted or pending creation).
    pub(crate) fn file_exists(&self, path: &Path) -> bool {
        let mut exists = self.persisted_files.contains_key(path);
        for op in &self.pending {
            match op {
                PendingOp::CreateFile { path: p, .. } if p == path => exists = true,
                PendingOp::CreateHardLink { path: p, .. } if p == path => exists = true,
                PendingOp::RemoveFile { path: p } if p == path => exists = false,
                PendingOp::Rename { from, to: _ } if from == path => exists = false,
                PendingOp::Rename { from: _, to } if to == path => exists = true,
                _ => {}
            }
        }
        exists
    }

    /// Check if a directory exists (persisted or pending creation).
    pub(crate) fn dir_exists(&self, path: &Path) -> bool {
        let mut exists = self.persisted_dirs.contains_key(path);
        for op in &self.pending {
            match op {
                PendingOp::CreateDir { path: p, .. } if p == path => exists = true,
                PendingOp::RemoveDir { path: p } if p == path => exists = false,
                // For renames, we need to check if the source was a directory
                PendingOp::Rename { from, to: _ } if from == path => {
                    // Source directory is being renamed away
                    if self.persisted_dirs.contains_key(from) {
                        exists = false;
                    }
                }
                PendingOp::Rename { from, to } if to == path => {
                    // Something is being renamed to this path - only mark as directory
                    // if the source was a directory
                    if self.persisted_dirs.contains_key(from) {
                        exists = true;
                    }
                }
                _ => {}
            }
        }
        exists
    }

    /// Check if a symlink exists (persisted or pending creation).
    pub(crate) fn symlink_exists(&self, path: &Path) -> bool {
        let mut exists = self.persisted_symlinks.contains_key(path);
        for op in &self.pending {
            match op {
                PendingOp::CreateSymlink { path: p, .. } if p == path => exists = true,
                PendingOp::RemoveFile { path: p } if p == path => exists = false,
                // For renames, we need to check if the source was a symlink
                PendingOp::Rename { from, to: _ } if from == path => {
                    // Source symlink is being renamed away - it no longer exists at 'from'
                    // Check both persisted and if 'exists' was set by a prior pending create
                    if self.persisted_symlinks.contains_key(from) || exists {
                        exists = false;
                    }
                }
                PendingOp::Rename { from, to } if to == path => {
                    // Something is being renamed to this path - only mark as symlink
                    // if the source was a symlink (persisted or pending)
                    if self.persisted_symlinks.contains_key(from)
                        || self.was_symlink_created_at(from)
                    {
                        exists = true;
                    }
                }
                _ => {}
            }
        }
        exists
    }

    /// Helper to check if a symlink was created at a path in pending ops.
    fn was_symlink_created_at(&self, path: &Path) -> bool {
        for op in &self.pending {
            if let PendingOp::CreateSymlink { path: p, .. } = op {
                if p == path {
                    return true;
                }
            }
        }
        false
    }

    /// Get current file length (persisted + pending).
    pub(crate) fn file_len(&self, path: &Path) -> u64 {
        // Resolve to the content path (handles hard links and renames)
        let content_path = self.resolve_content_path(path);
        let mut len = self
            .persisted_files
            .get(&content_path)
            .map(|v| v.content.len() as u64)
            .unwrap_or(0);

        for op in &self.pending {
            match op {
                PendingOp::Write {
                    path: p,
                    offset,
                    data,
                    ..
                } => {
                    // Check if this write applies to the content path
                    if p == &content_path || self.path_renamed_to(p, &content_path) {
                        let end = offset + data.len() as u64;
                        if end > len {
                            len = end;
                        }
                    }
                }
                PendingOp::SetLen {
                    path: p,
                    len: new_len,
                    ..
                } => {
                    if p == &content_path || self.path_renamed_to(p, &content_path) {
                        len = *new_len;
                    }
                }
                _ => {}
            }
        }
        len
    }

    /// Create a directory. Returns error if parent doesn't exist or dir already exists.
    pub(crate) fn mkdir(&mut self, path: &Path, time: Duration) -> Result<(), &'static str> {
        self.mkdir_with_mode(path, time, 0o755)
    }

    /// Create a directory with specified mode.
    pub(crate) fn mkdir_with_mode(
        &mut self,
        path: &Path,
        time: Duration,
        mode: u32,
    ) -> Result<(), &'static str> {
        // Check parent exists
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !self.dir_exists(parent) {
                return Err("No such file or directory");
            }
        }

        // Check doesn't already exist
        if self.dir_exists(path) || self.file_exists(path) || self.symlink_exists(path) {
            return Err("File exists");
        }

        self.pending.push(PendingOp::CreateDir {
            path: path.to_path_buf(),
            time,
            mode,
        });
        Ok(())
    }

    /// Remove an empty directory.
    pub(crate) fn rmdir(&mut self, path: &Path) -> Result<(), &'static str> {
        if !self.dir_exists(path) {
            return Err("No such file or directory");
        }

        // Check directory is empty
        if self.dir_has_children(path) {
            return Err("Directory not empty");
        }

        self.pending.push(PendingOp::RemoveDir {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    /// Check if directory has any children (files, subdirs, or symlinks).
    fn dir_has_children(&self, path: &Path) -> bool {
        // Check persisted files that still exist
        for file_path in self.persisted_files.keys() {
            if file_path.parent() == Some(path) && self.file_exists(file_path) {
                return true;
            }
        }
        // Check persisted dirs that still exist
        for dir_path in self.persisted_dirs.keys() {
            if dir_path.parent() == Some(path) && self.dir_exists(dir_path) {
                return true;
            }
        }
        // Check persisted symlinks that still exist
        for symlink_path in self.persisted_symlinks.keys() {
            if symlink_path.parent() == Some(path) && self.symlink_exists(symlink_path) {
                return true;
            }
        }
        // Check pending creates (using exists checks to account for later removals)
        for op in &self.pending {
            match op {
                PendingOp::CreateFile { path: p, .. } if p.parent() == Some(path) => {
                    if self.file_exists(p) {
                        return true;
                    }
                }
                PendingOp::CreateDir { path: p, .. } if p.parent() == Some(path) => {
                    if self.dir_exists(p) {
                        return true;
                    }
                }
                PendingOp::CreateSymlink { path: p, .. } if p.parent() == Some(path) => {
                    if self.symlink_exists(p) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Check if parent directory exists for a file path.
    pub(crate) fn parent_exists(&self, path: &Path) -> bool {
        match path.parent() {
            None => true,
            Some(parent) if parent.as_os_str().is_empty() => true,
            Some(parent) => self.dir_exists(parent),
        }
    }

    /// Remove a file or symlink.
    pub(crate) fn unlink(&mut self, path: &Path) -> Result<(), &'static str> {
        if !self.file_exists(path) && !self.symlink_exists(path) {
            return Err("No such file or directory");
        }

        // Invalidate page cache for deleted file
        if let Some(cache) = &mut self.page_cache {
            cache.invalidate_file(path);
        }

        self.pending.push(PendingOp::RemoveFile {
            path: path.to_path_buf(),
        });
        Ok(())
    }

    /// Rename a file, directory, or symlink.
    pub(crate) fn rename(&mut self, from: &Path, to: &Path) -> Result<(), &'static str> {
        // Check destination parent exists
        if !self.parent_exists(to) {
            return Err("No such file or directory");
        }

        // Try renaming a file
        if self.file_exists(from) {
            // Can't rename file onto directory
            if self.dir_exists(to) {
                return Err("Is a directory");
            }
            self.pending.push(PendingOp::Rename {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
            });
            return Ok(());
        }

        // Try renaming a directory
        if self.dir_exists(from) {
            // Can't rename dir onto file or symlink
            if self.file_exists(to) || self.symlink_exists(to) {
                return Err("Not a directory");
            }
            // If destination dir exists, it must be empty
            if self.dir_exists(to) && self.dir_has_children(to) {
                return Err("Directory not empty");
            }
            self.pending.push(PendingOp::Rename {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
            });
            return Ok(());
        }

        // Try renaming a symlink
        if self.symlink_exists(from) {
            // Can't rename symlink onto directory
            if self.dir_exists(to) {
                return Err("Is a directory");
            }
            self.pending.push(PendingOp::Rename {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
            });
            return Ok(());
        }

        Err("No such file or directory")
    }

    /// Sync a file (makes file data and metadata durable).
    ///
    /// This is equivalent to fsync() on a file descriptor. It flushes:
    /// - File data (writes)
    /// - File size changes (truncate/extend)
    ///
    /// Note: This does NOT make the directory entry durable. To ensure
    /// a newly created file survives a crash, you must also call
    /// sync_dir() on the parent directory. Without the directory sync,
    /// the file's data is durable but may be orphaned (inaccessible) after crash.
    pub(crate) fn sync_file(&mut self, path: &Path) -> Result<(), &'static str> {
        if !self.file_exists(path) {
            return Err("No such file or directory");
        }

        // Flush pending ops that affect this file's DATA only
        // CreateFile is a directory entry op, handled by sync_dir
        let (to_flush, to_keep): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|op| match op {
                PendingOp::Write { path: p, .. } => p == path,
                PendingOp::SetLen { path: p, .. } => p == path,
                _ => false,
            });

        // Ensure file exists in persisted_files for writes to be applied
        // (CreateFile may still be pending in to_keep, but we need somewhere to write data)
        if !self.persisted_files.contains_key(path) {
            // Look for pending CreateFile in to_keep to get the creation timestamp
            let (crtime, mode) = to_keep
                .iter()
                .find_map(|op| match op {
                    PendingOp::CreateFile {
                        path: p,
                        time,
                        mode,
                    } if p == path => Some((*time, *mode)),
                    _ => None,
                })
                .unwrap_or((Duration::ZERO, 0o644));
            self.persisted_files
                .insert(path.to_path_buf(), FileData::with_mode(crtime, mode));
        }

        self.pending = to_keep;
        for op in &to_flush {
            self.apply_op_to_persisted(op);
        }
        Ok(())
    }

    /// Sync file data only (not metadata like creation).
    /// This is equivalent to fdatasync() - flushes data and size, but not creation.
    /// The file entry won't be durable until the parent directory is synced.
    pub(crate) fn sync_file_data(&mut self, path: &Path) -> Result<(), &'static str> {
        if !self.file_exists(path) {
            return Err("No such file or directory");
        }

        // Flush only data ops (Write, SetLen), NOT CreateFile
        let (to_flush, to_keep): (Vec<_>, Vec<_>) =
            self.pending.drain(..).partition(|op| match op {
                PendingOp::Write { path: p, .. } => p == path,
                PendingOp::SetLen { path: p, .. } => p == path,
                _ => false,
            });

        // Ensure file exists in persisted_files for data ops to be applied
        // (CreateFile may still be pending in to_keep)
        if !self.persisted_files.contains_key(path) {
            let (crtime, mode) = to_keep
                .iter()
                .find_map(|op| match op {
                    PendingOp::CreateFile {
                        path: p,
                        time,
                        mode,
                    } if p == path => Some((*time, *mode)),
                    _ => None,
                })
                .unwrap_or((Duration::ZERO, 0o644));
            self.persisted_files
                .insert(path.to_path_buf(), FileData::with_mode(crtime, mode));
        }

        self.pending = to_keep;
        for op in &to_flush {
            self.apply_op_to_persisted(op);
        }
        Ok(())
    }

    /// Sync a directory (makes directory entries durable).
    ///
    /// This flushes pending operations that affect the directory's contents:
    /// - File/directory creations in this directory
    /// - File/directory removals from this directory
    /// - Renames into/out of this directory
    /// - The directory's own creation (if pending)
    ///
    /// Note: This does NOT sync the file data of files in the directory.
    /// Each file must be synced separately with sync_file().
    pub(crate) fn sync_dir(&mut self, path: &Path, time: Duration) -> Result<(), &'static str> {
        if !self.dir_exists(path) {
            return Err("No such file or directory");
        }

        // Flush pending ops that affect this directory's entries
        let (to_flush, to_keep): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|op| {
            match op {
                // Directory's own creation
                PendingOp::CreateDir { path: p, .. } if p == path => true,
                // Files/dirs/symlinks/hardlinks created in this directory
                PendingOp::CreateFile { path: p, .. } => p.parent() == Some(path),
                PendingOp::CreateDir { path: p, .. } => p.parent() == Some(path),
                PendingOp::CreateSymlink { path: p, .. } => p.parent() == Some(path),
                PendingOp::CreateHardLink { path: p, .. } => p.parent() == Some(path),
                // Files/dirs removed from this directory
                PendingOp::RemoveFile { path: p } => p.parent() == Some(path),
                PendingOp::RemoveDir { path: p } => p.parent() == Some(path),
                // Renames affecting this directory
                PendingOp::Rename { from, to } => {
                    from.parent() == Some(path) || to.parent() == Some(path)
                }
                _ => false,
            }
        });

        self.pending = to_keep;

        // Track if we need to update this directory's mtime
        let mut dir_modified = false;

        for op in &to_flush {
            // Check if this op modifies our directory's contents and update synced_entries
            match op {
                PendingOp::CreateFile { path: p, .. } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.insert(p.clone());
                }
                PendingOp::CreateDir { path: p, .. } if p == path => {
                    // Directory's own creation - mark it as synced
                    self.synced_entries.insert(p.clone());
                }
                PendingOp::CreateDir { path: p, .. } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.insert(p.clone());
                }
                PendingOp::CreateSymlink { path: p, .. } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.insert(p.clone());
                }
                PendingOp::CreateHardLink { path: p, .. } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.insert(p.clone());
                }
                PendingOp::RemoveFile { path: p } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.swap_remove(p);
                }
                PendingOp::RemoveDir { path: p } if p.parent() == Some(path) => {
                    dir_modified = true;
                    self.synced_entries.swap_remove(p);
                }
                PendingOp::Rename { from, to } => {
                    if from.parent() == Some(path) {
                        dir_modified = true;
                        self.synced_entries.swap_remove(from);
                    }
                    if to.parent() == Some(path) {
                        dir_modified = true;
                        self.synced_entries.insert(to.clone());
                    }
                }
                _ => {}
            }
            self.apply_op_to_persisted(op);
        }

        // Update directory mtime if contents changed
        if dir_modified {
            if let Some(dir_data) = self.persisted_dirs.get_mut(path) {
                dir_data.mtime = time;
                dir_data.ctime = time;
            }
        }

        Ok(())
    }

    /// Read file content at offset, merging persisted + pending.
    pub(crate) fn read_file(&self, path: &Path, buf: &mut [u8], offset: u64) -> usize {
        if buf.is_empty() {
            return 0;
        }

        let file_len = self.file_len(path);
        if offset >= file_len {
            return 0;
        }

        let available = (file_len - offset) as usize;
        let to_read = buf.len().min(available);

        // Start with zeros
        buf[..to_read].fill(0);

        // Resolve to the content path (handles hard links and renames)
        let content_path = self.resolve_content_path(path);

        // Copy persisted data in range
        if let Some(file_data) = self.persisted_files.get(&content_path) {
            let persisted_end = (file_data.content.len() as u64).min(offset + to_read as u64);
            if offset < persisted_end {
                let src_start = offset as usize;
                let src_end = persisted_end as usize;
                let dst_len = src_end - src_start;
                buf[..dst_len].copy_from_slice(&file_data.content[src_start..src_end]);
            }
        }

        // Overlay pending writes (need to check the content path)
        for op in &self.pending {
            if let PendingOp::Write {
                path: p,
                offset: write_off,
                data,
                ..
            } = op
            {
                // Check if this write applies to the content path
                let write_applies = p == &content_path || self.path_renamed_to(p, &content_path);
                if write_applies {
                    let write_end = write_off + data.len() as u64;
                    let read_end = offset + to_read as u64;
                    if *write_off < read_end && write_end > offset {
                        let overlap_start = write_off.max(&offset);
                        let overlap_end = write_end.min(read_end);
                        let src_offset = (overlap_start - write_off) as usize;
                        let dst_offset = (overlap_start - offset) as usize;
                        let len = (overlap_end - overlap_start) as usize;
                        buf[dst_offset..dst_offset + len]
                            .copy_from_slice(&data[src_offset..src_offset + len]);
                    }
                }
            }
        }

        to_read
    }

    /// Check if `from` was renamed to `to` via pending ops.
    fn path_renamed_to(&self, from: &Path, to: &Path) -> bool {
        let mut current = from.to_path_buf();
        for op in &self.pending {
            if let PendingOp::Rename { from: f, to: t } = op {
                if f == &current {
                    current = t.clone();
                }
            }
        }
        current == to
    }

    /// Write to a file (adds to pending queue).
    pub(crate) fn write_file(&mut self, path: &Path, offset: u64, data: &[u8], time: Duration) {
        if !data.is_empty() {
            self.pending.push(PendingOp::Write {
                path: path.to_path_buf(),
                offset,
                data: data.to_vec(),
                time,
            });
        }
    }

    /// Set file length (adds to pending queue).
    pub(crate) fn set_file_len(&mut self, path: &Path, len: u64, time: Duration) {
        // Invalidate page cache when truncating (pages beyond new length are stale)
        // We invalidate all pages for simplicity; a more sophisticated impl could
        // invalidate only pages >= len / page_size.
        if let Some(cache) = &mut self.page_cache {
            cache.invalidate_file(path);
        }

        self.pending.push(PendingOp::SetLen {
            path: path.to_path_buf(),
            len,
            time,
        });
    }

    /// Create a file (adds to pending queue).
    #[allow(dead_code)]
    pub(crate) fn create_file(&mut self, path: &Path, time: Duration) {
        self.create_file_with_mode(path, time, 0o644);
    }

    /// Create a file with specified mode (adds to pending queue).
    pub(crate) fn create_file_with_mode(&mut self, path: &Path, time: Duration, mode: u32) {
        self.pending.push(PendingOp::CreateFile {
            path: path.to_path_buf(),
            time,
            mode,
        });
    }

    /// Create a symlink (adds to pending queue).
    pub(crate) fn create_symlink(
        &mut self,
        path: &Path,
        target: &Path,
        time: Duration,
    ) -> Result<(), &'static str> {
        // Check parent exists
        if !self.parent_exists(path) {
            return Err("No such file or directory");
        }

        // Check nothing exists at path
        if self.file_exists(path) || self.dir_exists(path) || self.symlink_exists(path) {
            return Err("File exists");
        }

        self.pending.push(PendingOp::CreateSymlink {
            path: path.to_path_buf(),
            target: target.to_path_buf(),
            time,
        });
        Ok(())
    }

    /// Create a hard link (adds to pending queue).
    pub(crate) fn create_hard_link(
        &mut self,
        path: &Path,
        target: &Path,
        time: Duration,
    ) -> Result<(), &'static str> {
        // Check parent exists
        if !self.parent_exists(path) {
            return Err("No such file or directory");
        }

        // Check target exists and is a file
        if !self.file_exists(target) {
            return Err("No such file or directory");
        }

        // Check nothing exists at path
        if self.file_exists(path) || self.dir_exists(path) || self.symlink_exists(path) {
            return Err("File exists");
        }

        self.pending.push(PendingOp::CreateHardLink {
            path: path.to_path_buf(),
            target: target.to_path_buf(),
            time,
        });
        Ok(())
    }

    /// Read symlink target.
    pub(crate) fn read_link(&self, path: &Path) -> Result<PathBuf, &'static str> {
        // Resolve path through renames (walk backwards to find original path)
        let original_path = self.resolve_symlink_path(path);

        // Check pending ops first (most recent) for symlink creation at original path
        for op in self.pending.iter().rev() {
            match op {
                PendingOp::CreateSymlink {
                    path: p, target, ..
                } if p == &original_path => {
                    return Ok(target.clone());
                }
                PendingOp::RemoveFile { path: p } if p == &original_path => {
                    return Err("No such file or directory");
                }
                _ => {}
            }
        }

        // Check persisted at original path
        self.persisted_symlinks
            .get(&original_path)
            .map(|s| s.target.clone())
            .ok_or("No such file or directory")
    }

    /// Resolve a symlink path through pending renames.
    /// If path was created via rename, returns the original source path.
    fn resolve_symlink_path(&self, path: &Path) -> PathBuf {
        let mut current = path.to_path_buf();
        // Walk backwards through renames to find the original path
        for op in self.pending.iter().rev() {
            if let PendingOp::Rename { from, to } = op {
                if to == &current {
                    current = from.clone();
                }
            }
        }
        current
    }

    /// Set file/directory permissions.
    pub(crate) fn set_permissions(
        &mut self,
        path: &Path,
        mode: u32,
        time: Duration,
    ) -> Result<(), &'static str> {
        if !self.file_exists(path) && !self.dir_exists(path) {
            return Err("No such file or directory");
        }
        self.pending.push(PendingOp::SetPermissions {
            path: path.to_path_buf(),
            mode,
            time,
        });
        Ok(())
    }

    /// Get file permissions.
    #[allow(dead_code)]
    pub(crate) fn file_mode(&self, path: &Path) -> Option<u32> {
        // Check pending ops first
        for op in self.pending.iter().rev() {
            match op {
                PendingOp::SetPermissions { path: p, mode, .. } if p == path => {
                    return Some(*mode);
                }
                PendingOp::CreateFile { path: p, mode, .. } if p == path => {
                    return Some(*mode);
                }
                _ => {}
            }
        }
        self.persisted_files.get(path).map(|f| f.mode)
    }

    /// Get directory permissions.
    #[allow(dead_code)]
    pub(crate) fn dir_mode(&self, path: &Path) -> Option<u32> {
        // Check pending ops first
        for op in self.pending.iter().rev() {
            match op {
                PendingOp::SetPermissions { path: p, mode, .. } if p == path => {
                    return Some(*mode);
                }
                PendingOp::CreateDir { path: p, mode, .. } if p == path => {
                    return Some(*mode);
                }
                _ => {}
            }
        }
        self.persisted_dirs.get(path).map(|d| d.mode)
    }

    /// Get nlink count for a file, accounting for pending hard links.
    ///
    /// For hard links, returns the shared nlink count (target's nlink).
    pub(crate) fn file_nlink(&self, path: &Path) -> u64 {
        // If this file is a pending hard link, get its target's nlink
        if let Some(target) = self.resolve_hardlink_target(path) {
            return self.file_nlink(&target);
        }

        let mut nlink = self.persisted_files.get(path).map(|f| f.nlink).unwrap_or(1);

        // Count pending hard links that reference this file as target
        for op in &self.pending {
            if let PendingOp::CreateHardLink { target, .. } = op {
                if target == path {
                    nlink += 1;
                }
            }
        }
        nlink
    }

    /// Get timestamps for a file.
    /// Returns (crtime, mtime, ctime) for the file, accounting for pending operations.
    pub(crate) fn file_timestamps(&self, path: &Path) -> Option<(Duration, Duration, Duration)> {
        // Start with persisted timestamps
        let persisted_path = self.resolve_persisted_path(path);
        let mut timestamps = self
            .persisted_files
            .get(&persisted_path)
            .map(|f| (f.crtime, f.mtime, f.ctime));

        // Apply pending ops that affect timestamps
        for op in &self.pending {
            match op {
                PendingOp::CreateFile { path: p, time, .. } if p == path => {
                    // New file creation sets all timestamps
                    timestamps = Some((*time, *time, *time));
                }
                PendingOp::Write { path: p, time, .. }
                | PendingOp::SetLen { path: p, time, .. } => {
                    // Writes and truncates update mtime/ctime
                    if (p == path || self.path_renamed_to(p, path)) && timestamps.is_some() {
                        let (crtime, _, _) = timestamps.unwrap();
                        timestamps = Some((crtime, *time, *time));
                    }
                }
                _ => {}
            }
        }

        timestamps
    }

    /// Get timestamps for a directory.
    /// Returns (crtime, mtime, ctime) for the directory, accounting for pending operations.
    ///
    /// Directory mtime is updated when:
    /// - The directory itself is created
    /// - Files or subdirectories are created in the directory
    /// - Files or subdirectories are removed from the directory
    /// - Files or subdirectories are renamed into/out of the directory
    pub(crate) fn dir_timestamps(&self, path: &Path) -> Option<(Duration, Duration, Duration)> {
        // Start with persisted timestamps
        let mut timestamps = self
            .persisted_dirs
            .get(path)
            .map(|d| (d.crtime, d.mtime, d.ctime));

        // Apply pending ops that affect timestamps
        for op in &self.pending {
            match op {
                PendingOp::CreateDir { path: p, time, .. } if p == path => {
                    // New dir creation sets all timestamps
                    timestamps = Some((*time, *time, *time));
                }
                PendingOp::CreateFile { path: p, time, .. } if p.parent() == Some(path) => {
                    // File created in this directory updates mtime
                    if let Some((crtime, _, _)) = timestamps {
                        timestamps = Some((crtime, *time, *time));
                    }
                }
                PendingOp::CreateDir { path: p, time, .. } if p.parent() == Some(path) => {
                    // Subdir created in this directory updates mtime
                    if let Some((crtime, _, _)) = timestamps {
                        timestamps = Some((crtime, *time, *time));
                    }
                }
                PendingOp::RemoveFile { path: p } if p.parent() == Some(path) => {
                    // File removed from this directory updates mtime
                    // Use current latest mtime as proxy (removal doesn't carry timestamp)
                    // In real fs, this would use current time
                    if let Some((crtime, mtime, _)) = timestamps {
                        timestamps = Some((crtime, mtime, mtime));
                    }
                }
                PendingOp::RemoveDir { path: p } if p.parent() == Some(path) => {
                    // Subdir removed from this directory updates mtime
                    if let Some((crtime, mtime, _)) = timestamps {
                        timestamps = Some((crtime, mtime, mtime));
                    }
                }
                PendingOp::Rename { from, to } => {
                    // Rename affects both source and destination parent directories
                    if from.parent() == Some(path) || to.parent() == Some(path) {
                        if let Some((crtime, mtime, _)) = timestamps {
                            timestamps = Some((crtime, mtime, mtime));
                        }
                    }
                }
                _ => {}
            }
        }

        timestamps
    }

    /// Get timestamps for a symlink.
    /// Returns (crtime, mtime, ctime) for the symlink, accounting for pending operations.
    pub(crate) fn symlink_timestamps(&self, path: &Path) -> Option<(Duration, Duration, Duration)> {
        // Start with persisted timestamps
        let mut timestamps = self
            .persisted_symlinks
            .get(path)
            .map(|s| (s.crtime, s.mtime, s.ctime));

        // Apply pending ops that affect timestamps
        for op in &self.pending {
            if let PendingOp::CreateSymlink { path: p, time, .. } = op {
                if p == path {
                    timestamps = Some((*time, *time, *time));
                }
            }
        }

        timestamps
    }

    /// List entries in a directory.
    /// Returns paths of files, directories, and symlinks that are direct children of the given path.
    pub(crate) fn dir_entries(&self, path: &Path) -> Vec<PathBuf> {
        use std::collections::HashSet;

        let mut entries: HashSet<PathBuf> = HashSet::new();

        // Add persisted files in this directory
        for file_path in self.persisted_files.keys() {
            if file_path.parent() == Some(path) && self.file_exists(file_path) {
                entries.insert(file_path.clone());
            }
        }

        // Add persisted directories in this directory
        for dir_path in self.persisted_dirs.keys() {
            if dir_path.parent() == Some(path) && self.dir_exists(dir_path) {
                entries.insert(dir_path.clone());
            }
        }

        // Add persisted symlinks in this directory
        for symlink_path in self.persisted_symlinks.keys() {
            if symlink_path.parent() == Some(path) && self.symlink_exists(symlink_path) {
                entries.insert(symlink_path.clone());
            }
        }

        // Add pending creations
        for op in &self.pending {
            match op {
                PendingOp::CreateFile { path: p, .. } if p.parent() == Some(path) => {
                    if self.file_exists(p) {
                        entries.insert(p.clone());
                    }
                }
                PendingOp::CreateDir { path: p, .. } if p.parent() == Some(path) => {
                    if self.dir_exists(p) {
                        entries.insert(p.clone());
                    }
                }
                PendingOp::CreateSymlink { path: p, .. } if p.parent() == Some(path) => {
                    if self.symlink_exists(p) {
                        entries.insert(p.clone());
                    }
                }
                PendingOp::Rename { to, .. } if to.parent() == Some(path) => {
                    // Check if it still exists (wasn't removed later)
                    if self.file_exists(to) || self.dir_exists(to) || self.symlink_exists(to) {
                        entries.insert(to.clone());
                    }
                }
                _ => {}
            }
        }

        entries.into_iter().collect()
    }
}

impl Default for Fs {
    fn default() -> Self {
        Self::new(FsConfig::default())
    }
}
