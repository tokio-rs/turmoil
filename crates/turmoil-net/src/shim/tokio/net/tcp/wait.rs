//! Per-direction wait lists for a [`TcpStream`].
//!
//! The kernel parks one waker per direction for each stream: a fan-out
//! waker owned by the stream that wakes every task waiting on it.
//! Async waits (`readable`, `ready`, `peek`, ...) each hold an entry
//! that is removed when the future completes or is dropped, so a
//! cancelled wait leaves no task waker behind. `poll_*` methods share a
//! single slot per direction, as tokio's do: only the most recent
//! caller is woken.
//!
//! [`TcpStream`]: super::TcpStream

use std::sync::{Arc, Mutex};
use std::task::{Wake, Waker};

use indexmap::IndexMap;

#[derive(Debug, Default)]
struct List {
    /// Shared slot for `poll_*` callers.
    poll: Option<Waker>,
    /// One entry per pending async wait, in registration order. `None`
    /// once woken, until the future polls again.
    waiters: IndexMap<u64, Option<Waker>>,
    next_id: u64,
}

#[derive(Debug, Default)]
struct WaitList(Mutex<List>);

impl Wake for WaitList {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // Collect first: a woken task may poll (and lock) inline.
        let wakers: Vec<Waker> = {
            let mut list = self.0.lock().unwrap();
            let mut wakers: Vec<Waker> = list.poll.take().into_iter().collect();
            wakers.extend(list.waiters.values_mut().filter_map(Option::take));
            wakers
        };
        for waker in wakers {
            waker.wake();
        }
    }
}

/// One direction (read or write) of a stream.
#[derive(Debug)]
pub(crate) struct Direction {
    list: Arc<WaitList>,
    /// Fan-out waker over `list`, the only waker the kernel sees.
    waker: Waker,
}

impl Direction {
    pub(crate) fn new() -> Self {
        let list = Arc::new(WaitList::default());
        let waker = Waker::from(list.clone());
        Self { list, waker }
    }

    /// The waker to register with the kernel.
    pub(crate) fn waker(&self) -> &Waker {
        &self.waker
    }

    /// Record `waker` in the shared `poll_*` slot, replacing the
    /// previous caller.
    pub(crate) fn set_poll(&self, waker: &Waker) {
        let mut list = self.list.0.lock().unwrap();
        if !list.poll.as_ref().is_some_and(|w| w.will_wake(waker)) {
            list.poll = Some(waker.clone());
        }
    }

    /// Start an async wait. The entry lives until the returned guard
    /// drops.
    pub(crate) fn waiter(&self) -> Waiter<'_> {
        let mut list = self.list.0.lock().unwrap();
        let id = list.next_id;
        list.next_id += 1;
        list.waiters.insert(id, None);
        Waiter { dir: self, id }
    }
}

/// An async wait registered on a [`Direction`].
pub(crate) struct Waiter<'a> {
    dir: &'a Direction,
    id: u64,
}

impl Waiter<'_> {
    /// Record the task to wake. Called on every poll, since a future
    /// may move between tasks.
    pub(crate) fn set(&self, waker: &Waker) {
        let mut list = self.dir.list.0.lock().unwrap();
        let slot = list.waiters.get_mut(&self.id).expect("waiter registered");
        if !slot.as_ref().is_some_and(|w| w.will_wake(waker)) {
            *slot = Some(waker.clone());
        }
    }
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        let mut list = self.dir.list.0.lock().unwrap();
        list.waiters.shift_remove(&self.id);
    }
}
