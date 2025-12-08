//! Barriers allow tests to granularly observe and control the execution of
//! source code by injecting observability and control hooks into source code.
//!
//! Barriers allow construction of complex tests which otherwise may rely on
//! timing conditions (which are difficult to write, flaky, and hard to
//! maintain) or monitoring and control of the network layer.
//!
//! Barriers are designed for Turmoil simulation tests. They allow test code
//! to step the simulation until a barrier is triggered, and optionally suspend
//! source code execution until the test is ready to proceed.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────┐                              ┌─────────────────────┐
//! │ Source Code  │                              │ Test Code           │
//! │              │                              │                     │
//! │  ┌─────────┐ │      ┌──────────────────┐    │                     │
//! │  │ Trigger ┼─┼─────►│   Barrier Repo   │◄───┼── Barrier::build()  │
//! │  └─────────┘ │      │  (Thread Local)  │    │                     │
//! │  ┌─────────┐ │  ┌───┼                  ├────┼─► Barrier::wait()   │
//! │  │ Resumer │◄┼──┘   └──────────────────┘    │                     │
//! │  └─────────┘ │                              │                     │
//! │              │                              │                     │
//! └──────────────┘                              └─────────────────────┘
//! ```
//!
//! A barrier consists of two halves: a `Trigger` which defines the condition
//! a barrier is waiting for, and a `Resumer` which controls when the source
//! code in a barrier is released. Interesting points of source code may be
//! annotated with triggers; these triggers will no-op if test code is not
//! interested and are conditionally compiled out of non-test code.
//!
//! When test code creates a barrier, the condition and resumer is registered
//! in the barrier repo. Most barriers are 'observe-only' and do not control
//! execution (typically test code is simply driving simulation forward until
//! a Barrier is triggered). However, test code may cause a future hitting a
//! barrier to suspend until the test code resumes it. It can also cause the
//! code to panic, if testing how panics are handled is desired.
//!
//! Triggers are type-safe Rust structs. Source code may define triggers as any
//! type desired. Barrier conditions are defined as closures that match against
//! a trigger. Reactions are built as an enum of well-defined actions; arbitrary
//! reaction code is not allowed to curtail insane usage.
//!
//! Source code can use either [`trigger()`] (async, supports suspension) or
//! [`trigger_noop()`] (sync, only for observation) depending on whether the
//! execution flow needs to be potentially suspended by test code.
//!
//! Note: Each trigger event wakes at most one barrier and processes in order
//! of registration. Avoid registering multiple barriers for the same triggers
//! to avoid confusion.
//!
//! # Example
//!
//! ```ignore
//! // In source code (conditionally compiled for simulation)
//! async fn handle_prepare_ack(prepare_ack: PrepareAck) {
//!     // Processing...
//!
//!     #[cfg(feature = "turmoil-barriers")]
//!     turmoil::barriers::trigger(
//!         MyBarriers::PrepareAckReceived(prepare_ack.tx_id)
//!     ).await;
//!
//!     // Continue processing
//! }
//!
//! // In test code:
//! #[test]
//! fn test_prepare_ack_handling() {
//!     let mut sim = turmoil::Builder::new().build();
//!
//!     // Register a barrier which will suspend when condition matches
//!     let mut barrier = Barrier::build(
//!         Reaction::Suspend,
//!         move |t: &MyBarriers| {
//!             matches!(t, MyBarriers::PrepareAckReceived(id) if *id == expected_tx_id)
//!         }
//!     );
//!
//!     sim.client("test", async move {
//!         // Trigger the function being tested
//!         handle_prepare_ack(PrepareAck { tx_id: expected_tx_id }).await;
//!         Ok(())
//!     });
//!
//!     // Step simulation until barrier is triggered
//!     // Source code is now suspended at the trigger point
//!     let triggered = barrier.step_until_triggered(&mut sim).unwrap();
//!
//!     // When ready to continue source execution, drop triggered
//!     drop(triggered);
//!
//!     sim.run().unwrap();
//! }
//! ```

use std::{any::Any, cell::RefCell, marker::PhantomData, ops::Deref};

use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    oneshot,
};
use uuid::Uuid;

thread_local! {
    static BARRIERS: BarrierRepo = BarrierRepo::new();
}

struct BarrierRepo {
    barriers: RefCell<Vec<BarrierState>>,
}

impl BarrierRepo {
    fn new() -> Self {
        Self {
            barriers: RefCell::new(vec![]),
        }
    }

    fn insert(&self, barrier: BarrierState) {
        self.barriers.borrow_mut().push(barrier);
    }

    fn drop(&self, id: Uuid) {
        self.barriers.borrow_mut().retain(|t| t.id != id);
    }

    fn barrier<T: Any + Send>(&self, t: &T) -> Option<(Reaction, UnboundedSender<Waker>)> {
        let guard = self.barriers.borrow();
        for barrier in guard.iter() {
            if (barrier.condition)(t) {
                return Some((barrier.reaction.clone(), barrier.to_test.clone()));
            }
        }
        None
    }
}

/// Trigger a barrier (if any registered) with the given value.
///
/// Use this function when you need to give test code the ability to suspend
/// source execution at trigger points. Supports both observation ([`Reaction::Noop`])
/// and suspension ([`Reaction::Suspend`]) of execution flow.
///
/// If you only need to notify without suspension capability, use [`trigger_noop()`]
/// instead.
///
/// This function is a no-op if no barrier is registered for the given trigger type.
pub async fn trigger<T: Any + Send>(t: T) {
    let Some((reaction, to_test)) = BARRIERS.with(|barriers| barriers.barrier(&t)) else {
        return;
    };

    let (tx, rx) = oneshot::channel();
    let waker = match reaction {
        Reaction::Noop => {
            tx.send(()).expect("Receiver is owned");
            None
        }
        Reaction::Suspend => Some(tx),
        Reaction::Panic => panic!("Injected panic from barrier"),
    };

    let _ = to_test.send((Box::new(t), waker));
    let _ = rx.await;
}

/// Synchronously trigger a barrier with the given value.
///
/// Use this function when you need to notify barriers about events without
/// suspending execution. Only supports [`Reaction::Noop`] reactions and will
/// panic if used with a barrier configured with [`Reaction::Suspend`].
///
/// For suspension capability, use [`trigger()`] instead.
///
/// This function is a no-op if no barrier is registered for the given trigger type.
pub fn trigger_noop<T: Any + Send>(t: T) {
    let Some((reaction, to_test)) = BARRIERS.with(|barriers| barriers.barrier(&t)) else {
        return;
    };

    if let Reaction::Suspend = reaction {
        panic!(
            "trigger_noop() cannot be used with Reaction::Suspend barriers; use trigger() instead"
        );
    }

    if let Reaction::Panic = reaction {
        panic!("Injected panic from barrier");
    }

    let _ = to_test.send((Box::new(t), None));
}

struct BarrierState {
    id: Uuid,
    condition: Box<Condition>,
    reaction: Reaction,
    to_test: UnboundedSender<Waker>,
}

type Condition = dyn Fn(&dyn Any) -> bool;

/// A barrier that waits for source code to trigger a specific condition.
///
/// Create barriers using [`Barrier::new()`] for observation-only barriers,
/// or [`Barrier::build()`] to specify a custom reaction.
///
/// # Example
///
/// ```ignore
/// // Wait for a specific event
/// let mut barrier = Barrier::new(|t: &MyEvent| {
///     matches!(t, MyEvent::SomethingHappened)
/// });
///
/// // ... run simulation ...
///
/// let triggered = barrier.wait().await.unwrap();
/// println!("Event occurred: {:?}", *triggered);
/// ```
pub struct Barrier<T> {
    id: Uuid,
    from_src: UnboundedReceiver<Waker>,
    _t: PhantomData<T>,
}

impl<T: Any + Send> Barrier<T> {
    /// Create a new observation-only barrier that matches the given condition.
    ///
    /// This is equivalent to `Barrier::build(Reaction::Noop, condition)`.
    pub fn new(condition: impl Fn(&T) -> bool + 'static) -> Self {
        Self::build(Reaction::Noop, condition)
    }

    /// Create a barrier with a specific reaction and condition.
    ///
    /// # Arguments
    ///
    /// * `reaction` - What happens when the barrier is triggered
    /// * `condition` - A closure that returns true when the trigger matches
    pub fn build(reaction: Reaction, condition: impl Fn(&T) -> bool + 'static) -> Self {
        let condition = Box::new(move |t: &dyn Any| match t.downcast_ref::<T>() {
            Some(t) => condition(t),
            None => false,
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let id = Uuid::new_v4();
        let state = BarrierState {
            id,
            condition,
            reaction,
            to_test: tx,
        };
        BARRIERS.with(|barriers| barriers.insert(state));
        Self {
            id,
            from_src: rx,
            _t: PhantomData,
        }
    }

    /// Wait for the barrier to be triggered.
    ///
    /// Returns `Some(Triggered<T>)` when the barrier is triggered, or `None`
    /// if the barrier is dropped.
    ///
    /// For [`Reaction::Suspend`] barriers, the source code remains suspended
    /// until the returned [`Triggered`] handle is dropped.
    pub async fn wait(&mut self) -> Option<Triggered<T>> {
        let (data, release) = self.from_src.recv().await?;
        let data = *data.downcast::<T>().unwrap();
        Some(Triggered { data, release })
    }
}

impl<T> Drop for Barrier<T> {
    fn drop(&mut self) {
        BARRIERS.with(|barriers| barriers.drop(self.id));
    }
}

/// A handle to a triggered barrier.
///
/// This struct holds the trigger value and controls when suspended source code
/// is released. For [`Reaction::Suspend`] barriers, dropping this handle will
/// resume the suspended source code.
///
/// Use [`Deref`] to access the trigger value.
pub struct Triggered<T> {
    data: T,
    release: Option<oneshot::Sender<()>>,
}

impl<T> Deref for Triggered<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> Drop for Triggered<T> {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

/// The reaction when a barrier is triggered.
#[derive(Debug, Clone)]
pub enum Reaction {
    /// Observe only - source code continues immediately after trigger.
    Noop,
    /// Suspend source code execution until the [`Triggered`] handle is dropped.
    Suspend,
    /// Cause a panic at the trigger point (useful for testing panic handling).
    Panic,
}

type Waker = (Box<dyn Any + Send>, Option<oneshot::Sender<()>>);
