//! Filesystem conformance suite.
//!
//! Linux is the oracle for the filesystem shim. The same cases run against
//! two backends, and each has to come to the same on both:
//!
//! - `sim`: `turmoil::fs::shim::std::fs`, on a simulated host.
//! - `real`: `std::fs`, in a scratch directory on the real filesystem.
//!
//! A line that differs is a gap in the shim. Run the suite on Linux with
//!
//! ```text
//! cargo test -p turmoil --features unstable-fs,real-fs-conformance --test fs_conformance
//! ```
//!
//! The scratch directory goes where `TMPDIR` says, which picks the
//! filesystem to compare with. The cases keep to what Linux filesystems
//! agree on.

#![cfg(all(target_os = "linux", feature = "real-fs-conformance"))]
// Each arm is the same cases, compiled against its own filesystem.
#![allow(clippy::duplicate_mod)]

#[path = "fs_conformance/real.rs"]
mod real;
#[path = "fs_conformance/sim.rs"]
mod sim;

use std::fs::{set_permissions, Permissions};
use std::mem;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;
use turmoil::fs::{Fs, FsContext};
use turmoil::Builder;

#[test]
fn shim_matches_the_real_filesystem() -> turmoil::Result {
    // A simulated process starts with the usual umask, so this one takes it.
    // SAFETY: umask(2) swaps the mask of the process and cannot fail. No
    // other test runs in this process.
    unsafe { libc::umask(0o022) };
    let scratch = tempfile::tempdir()?;
    // Where the scratch directory is must not show in the cases: neither a
    // symlink on the way to it, nor a setgid bit it took from its parent.
    let root = scratch.path().canonicalize()?;
    set_permissions(&root, Permissions::from_mode(0o700))?;
    let root = root.to_str().expect("a scratch path in UTF-8");
    // Real timestamps come off a coarse clock, of ten milliseconds at worst.
    let linux = real::cases::run(root, &|| sleep(Duration::from_millis(20)));

    let reported = Arc::new(Mutex::new(Vec::new()));
    let report = reported.clone();
    let mut sim = Builder::new().build();
    sim.client("shim", async move {
        let tick = || {
            let now = FsContext::current(|ctx| ctx.now);
            Fs::current().set_now(now + Duration::from_secs(1));
        };
        *report.lock().unwrap() = sim::cases::run("", &tick);
        Ok(())
    });
    sim.run()?;
    let shim = mem::take(&mut *reported.lock().unwrap());

    assert_eq!(linux.len(), shim.len());
    let differs = |(linux, shim): &(&String, &String)| linux != shim;
    let gap = |(linux, shim): (&String, &String)| format!("linux: {linux}\n shim: {shim}");
    let gaps: Vec<_> = linux.iter().zip(&shim).filter(differs).map(gap).collect();
    let summary = format!("{} of {} cases differ", gaps.len(), linux.len());
    assert!(gaps.is_empty(), "{summary}:\n{}", gaps.join("\n"));
    Ok(())
}
