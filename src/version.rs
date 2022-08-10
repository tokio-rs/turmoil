use std::cell::RefCell;
use std::net::SocketAddr;
use std::time::Duration;

/// A "dot" is a host + pair tuple. The term originated from the "dotted version
/// vector" [1].
///
/// [1]: https://riak.com/posts/technical/vector-clocks-revisited-part-2-dotted-version-vectors/index.html
#[derive(Debug, Copy, Clone, serde::Serialize)]
pub(crate) struct Dot {
    pub(crate) host: SocketAddr,
    pub(crate) version: u64,
}

struct Current {
    dot: Dot,
    /// How much time has elapsed
    elapsed: Duration,
}

thread_local!(static CURRENT: RefCell<Option<Current>> = RefCell::new(None));

pub(crate) fn set_current(dot: Dot, elapsed: Duration) {
    CURRENT.with(|current| *current.borrow_mut() = Some(Current { dot, elapsed }));
}

pub(crate) fn get_current() -> (Dot, Duration) {
    CURRENT.with(|current| {
        let current = current.borrow();
        let current = current.as_ref().unwrap();
        (current.dot, current.elapsed)
    })
}

pub(crate) fn take_current() -> Dot {
    CURRENT.with(|current| {
        current.borrow_mut().take().unwrap().dot
    })
}

pub(crate) fn inc_current() -> (Dot, Duration) {
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        let current = current.as_mut().unwrap();
        current.dot.version += 1;
        (current.dot, current.elapsed)
    })
}
