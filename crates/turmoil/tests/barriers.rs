//! Tests for the barriers module.
//!
//! Requires the `unstable-barriers` feature to be enabled.

#![cfg(feature = "unstable-barriers")]

use std::{ops::Deref, time::Duration};

use tokio::time::{sleep, timeout, Instant};
use turmoil::barriers::{trigger, trigger_noop, Barrier, Reaction};

#[derive(Debug, PartialEq, Eq)]
enum TestTrigger {
    First,
    Second,
    Third(usize),
}

/// Triggers without any registered barriers should be no-ops.
#[tokio::test]
async fn trigger_no_barrier() {
    trigger(TestTrigger::First).await;
    trigger(TestTrigger::Second).await;
    trigger(TestTrigger::Third(1738)).await;
}

/// Sync triggers without any registered barriers should be no-ops.
#[test]
fn trigger_noop_no_barrier() {
    trigger_noop(TestTrigger::First);
    trigger_noop(TestTrigger::Second);
    trigger_noop(TestTrigger::Third(1738));
}

/// Test that barriers can observe triggers.
#[tokio::test(start_paused = true)]
async fn barrier_observes_trigger() {
    tokio::spawn(async move {
        trigger(TestTrigger::First).await;
        sleep(Duration::from_secs(1)).await;
        trigger(TestTrigger::Third(1738)).await;
    });

    let mut barrier = Barrier::new(|t: &TestTrigger| t == &TestTrigger::Third(1738));
    let start = Instant::now();
    let triggered = barrier.wait().await.unwrap();
    assert_eq!(triggered.deref(), &TestTrigger::Third(1738));
    assert!(start.elapsed() > Duration::from_millis(100));
}

/// Test that dropping a barrier doesn't affect source code execution.
#[tokio::test(start_paused = true)]
async fn dropped_barrier() {
    let handle = tokio::spawn(async move {
        trigger(TestTrigger::First).await;
        sleep(Duration::from_secs(1)).await;
        trigger(TestTrigger::Third(1738)).await;
    });

    let mut barrier = Barrier::new(|t: &TestTrigger| t == &TestTrigger::Third(1738));
    let result = timeout(Duration::from_millis(100), barrier.wait()).await;
    assert!(result.is_err());
    drop(barrier);
    handle.await.unwrap();
}

/// Test that Suspend reaction holds source code until Triggered is dropped.
#[tokio::test(start_paused = true)]
async fn suspend_holds_execution() {
    let handle = tokio::spawn(async move {
        trigger(TestTrigger::Third(1738)).await;
    });

    let mut barrier = Barrier::build(Reaction::Suspend, |t: &TestTrigger| {
        t == &TestTrigger::Third(1738)
    });
    let triggered = barrier.wait().await.unwrap();
    assert_eq!(triggered.deref(), &TestTrigger::Third(1738));

    // Source should still be suspended
    sleep(Duration::from_secs(1)).await;
    assert!(!handle.is_finished());

    // Dropping triggered releases the suspension
    drop(triggered);
    handle.await.unwrap();
}

/// Test that Noop reaction doesn't hold execution.
#[tokio::test(start_paused = true)]
async fn noop_does_not_hold() {
    let handle = tokio::spawn(async move {
        trigger(TestTrigger::Third(1738)).await;
        // This should complete immediately after trigger
        42
    });

    let mut barrier = Barrier::new(|t: &TestTrigger| t == &TestTrigger::Third(1738));
    let _triggered = barrier.wait().await.unwrap();

    // Give the spawned task time to complete
    sleep(Duration::from_millis(1)).await;
    assert!(handle.is_finished());
    assert_eq!(handle.await.unwrap(), 42);
}

/// Test that barriers only match their specific condition.
#[tokio::test(start_paused = true)]
async fn barrier_matches_condition() {
    tokio::spawn(async move {
        trigger(TestTrigger::First).await;
        trigger(TestTrigger::Second).await;
        trigger(TestTrigger::Third(100)).await;
        trigger(TestTrigger::Third(200)).await;
    });

    // This barrier should only match Third(200)
    let mut barrier =
        Barrier::new(|t: &TestTrigger| matches!(t, TestTrigger::Third(n) if *n > 150));

    let triggered = barrier.wait().await.unwrap();
    assert_eq!(triggered.deref(), &TestTrigger::Third(200));
}

/// Test that trigger_noop works for observation.
#[tokio::test(start_paused = true)]
async fn trigger_noop_for_observation() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    tokio::spawn(async move {
        trigger_noop(TestTrigger::First);
        tx.send(()).await.unwrap();
    });

    let mut barrier = Barrier::new(|t: &TestTrigger| t == &TestTrigger::First);

    // Wait for the task to complete
    rx.recv().await.unwrap();

    // Give time for the barrier to process
    sleep(Duration::from_millis(1)).await;

    // The trigger should have been sent to the barrier
    let result = timeout(Duration::from_millis(10), barrier.wait()).await;
    assert!(result.is_ok());
}

/// Test that Panic reaction causes a panic.
#[tokio::test(start_paused = true)]
#[should_panic(expected = "Injected panic from barrier")]
async fn panic_reaction() {
    let _barrier = Barrier::build(Reaction::Panic, |t: &TestTrigger| t == &TestTrigger::First);
    trigger(TestTrigger::First).await;
}

/// Test that trigger_noop panics with Suspend reaction.
#[test]
#[should_panic(expected = "trigger_noop() cannot be used with Reaction::Suspend")]
fn trigger_noop_panics_with_suspend() {
    let _barrier = Barrier::build(Reaction::Suspend, |t: &TestTrigger| {
        t == &TestTrigger::First
    });
    trigger_noop(TestTrigger::First);
}

/// Test that trigger_noop panics with Panic reaction.
#[test]
#[should_panic(expected = "Injected panic from barrier")]
fn trigger_noop_with_panic_reaction() {
    let _barrier = Barrier::build(Reaction::Panic, |t: &TestTrigger| t == &TestTrigger::First);
    trigger_noop(TestTrigger::First);
}
