use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

use rand::{rngs::SmallRng, RngCore, SeedableRng};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil::{
    net::{TcpListener, TcpStream},
    Builder, Sim,
};

fn builder() -> Builder {
    let mut builder = Builder::new();
    builder
        .min_message_latency(Duration::from_millis(1))
        .max_message_latency(Duration::from_millis(500));
    builder
}

fn draws_of(seed: u64) -> impl FnMut() -> u64 {
    let mut rng = SmallRng::seed_from_u64(seed);
    move || rng.next_u64()
}

/// Runs an echo over TCP, calls `after_step` with the elapsed time after each
/// step, and returns the simulated time at which each answer arrived.
fn answered_at(mut sim: Sim<'_>, mut after_step: impl FnMut(Duration)) -> Vec<Duration> {
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:80").await?;
        let (mut stream, _) = listener.accept().await?;
        let mut byte = [0];
        while stream.read_exact(&mut byte).await.is_ok() {
            stream.write_all(&byte).await?;
        }
        Ok(())
    });

    let times = Rc::new(RefCell::new(Vec::new()));
    let answers = times.clone();
    sim.client("client", async move {
        let mut stream = TcpStream::connect("server:80").await?;
        let mut byte = [0];
        for _ in 0..8 {
            stream.write_all(&byte).await?;
            stream.read_exact(&mut byte).await?;
            answers.borrow_mut().push(turmoil::sim_elapsed().unwrap());
        }
        Ok(())
    });

    while !sim.step().unwrap() {
        after_step(sim.elapsed());
    }

    let answers = times.borrow().clone();
    answers
}

#[test]
fn the_simulation_draws_from_the_function() {
    let draws = Rc::new(Cell::new(0));
    let counted = draws.clone();
    let mut next_u64 = draws_of(1);
    let sim = builder().build_with_rng(move || {
        counted.set(counted.get() + 1);
        next_u64()
    });

    answered_at(sim, |_| {});

    assert!(draws.get() > 0);
}

#[test]
fn the_same_numbers_give_the_same_simulation() {
    let first = answered_at(builder().build_with_rng(draws_of(1)), |_| {});
    let second = answered_at(builder().build_with_rng(draws_of(1)), |_| {});
    let other = answered_at(builder().build_with_rng(draws_of(2)), |_| {});

    assert_eq!(first, second);
    assert_ne!(first, other);
}

#[test]
fn a_change_of_the_numbers_during_a_run_changes_only_what_comes_after_it() {
    let run = |mut change_at: Option<Duration>| {
        let rng = Rc::new(RefCell::new(SmallRng::seed_from_u64(1)));
        let draws = rng.clone();
        let sim = builder().build_with_rng(move || draws.borrow_mut().next_u64());
        answered_at(sim, |elapsed| {
            if change_at.is_some_and(|at| elapsed >= at) {
                *rng.borrow_mut() = SmallRng::seed_from_u64(2);
                change_at = None;
            }
        })
    };

    let whole = run(None);
    let changed = run(Some(whole[3]));

    assert_eq!(whole[..=3], changed[..=3]);
    assert_ne!(whole[4..], changed[4..]);
}
