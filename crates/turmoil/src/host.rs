#[cfg(feature = "unstable-fs")]
use crate::fs::{Fs, FsConfig};
#[cfg(feature = "unstable-io_uring")]
use crate::io_uring::host::IoUringHostState;

use std::net::IpAddr;
#[cfg(feature = "unstable-io_uring")]
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use turmoil_net::HostId;

/// A host in the simulated network.
///
/// Networking lives in the host's `turmoil_net` stack, identified by
/// `net`.
pub(crate) struct Host {
    /// Host ip address.
    pub(crate) addr: IpAddr,

    /// Tracks elapsed time for host and overall simulation.
    pub(crate) timer: HostTimer,

    /// The host's network stack.
    pub(crate) net: HostId,

    /// Crashed hosts are powered off: inbound packets are lost until
    /// the host is bounced.
    pub(crate) crashed: bool,

    /// Simulated filesystem.
    #[cfg(feature = "unstable-fs")]
    pub(crate) fs: Fs,

    /// Per-host io_uring registry: active rings + ring-fd allocator.
    #[cfg(feature = "unstable-io_uring")]
    pub(crate) io_uring: Arc<Mutex<IoUringHostState>>,
}

impl Host {
    #[cfg(feature = "unstable-fs")]
    pub(crate) fn new(
        addr: IpAddr,
        net: HostId,
        timer: HostTimer,
        fs_config: FsConfig,
        fs_seed: u64,
    ) -> Host {
        #[cfg(not(feature = "unstable-barriers"))]
        let builder = Fs::builder().config(fs_config).seed(fs_seed);
        #[cfg(feature = "unstable-barriers")]
        let builder = Fs::builder()
            .config(fs_config)
            .seed(fs_seed)
            .on_corruption(crate::fs_corruption_hook);
        Host {
            addr,
            net,
            crashed: false,
            fs: builder.build(),
            #[cfg(feature = "unstable-io_uring")]
            io_uring: Arc::new(Mutex::new(IoUringHostState::new())),
            timer,
        }
    }

    #[cfg(not(feature = "unstable-fs"))]
    pub(crate) fn new(addr: IpAddr, net: HostId, timer: HostTimer) -> Host {
        Host {
            addr,
            net,
            crashed: false,
            timer,
        }
    }
}

pub(crate) struct HostTimer {
    /// Host elapsed time.
    elapsed: Duration,

    /// Set each time the software is run.
    now: Option<Instant>,

    /// Time from the start of the simulation until this host was initialized.
    /// Used to calculate total simulation time below.
    start_offset: Duration,

    /// Simulation duration since unix epoch. Set when the simulation is
    /// created.
    since_epoch: Duration,
}

impl HostTimer {
    pub(crate) fn new(start_offset: Duration, since_epoch: Duration) -> Self {
        Self {
            elapsed: Duration::ZERO,
            now: None,
            start_offset,
            since_epoch,
        }
    }

    pub(crate) fn tick(&mut self, duration: Duration) {
        self.elapsed += duration
    }

    /// Set a new `Instant` for each iteration of the simulation. `elapsed` is
    /// updated after each iteration via `tick()`, where as this value is
    /// necessary to accurately calculate elapsed time while the software is
    /// running.
    ///
    /// This is required to track logical time across host restarts as a single
    /// `Instant` resets when the tokio runtime is recreated.
    pub(crate) fn now(&mut self, now: Instant) {
        self.now.replace(now);
    }

    /// Returns how long the host has been executing for in virtual time.
    pub(crate) fn elapsed(&self) -> Duration {
        let run_duration = self.now.expect("host instant not set").elapsed();
        self.elapsed + run_duration
    }

    /// Returns the total simulation virtual time. If the simulation ran for
    /// some time before this host was registered then sim_elapsed > elapsed.
    pub(crate) fn sim_elapsed(&self) -> Duration {
        self.start_offset + self.elapsed()
    }

    /// The logical duration from [`UNIX_EPOCH`] until now.
    ///
    /// On creation the simulation picks a `SystemTime` and calculates the
    /// duration since the epoch. Each `run()` invocation moves logical time
    /// forward the configured tick duration.
    pub(crate) fn since_epoch(&self) -> Duration {
        self.since_epoch + self.sim_elapsed()
    }
}
