//! `in_simulation`, `sim_elapsed`, and `since_epoch` must not allocate.
//!
//! These functions are useful for answering clock queries from code that
//! cannot tolerate allocation, such as a libc `clock_gettime` interposer that
//! the allocator itself may call while it holds internal locks or is midway
//! through a free. An allocation there re-enters the allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.with(Cell::get) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Returns the number of allocations made on this thread while running `f`.
fn allocations_during(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

fn query_clock() {
    std::hint::black_box(turmoil::in_simulation());
    std::hint::black_box(turmoil::sim_elapsed());
    std::hint::black_box(turmoil::since_epoch());
}

#[test]
fn clock_queries_outside_simulation_do_not_allocate() {
    assert!(!turmoil::in_simulation());
    assert_eq!(allocations_during(query_clock), 0);
}

#[test]
fn clock_queries_inside_host_do_not_allocate() -> turmoil::Result {
    let mut sim = turmoil::Builder::new().build();

    sim.client("client", async {
        assert!(turmoil::in_simulation());
        assert!(turmoil::since_epoch().is_some());
        assert_eq!(allocations_during(query_clock), 0);
        Ok(())
    });

    sim.run()
}
