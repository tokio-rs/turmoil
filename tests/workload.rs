use std::{
    cell::RefCell,
    collections::VecDeque,
    error::Error,
    ops::{Deref, DerefMut},
    rc::Rc,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};
use tracing_subscriber::EnvFilter;
use turmoil::{
    net::{TcpListener, TcpStream},
    workload::{self, CompoundWorkload},
    Builder, Sim,
};

struct Cluster {
    machines: Vec<String>,
    events: VecDeque<Event>,
}

enum Event {
    Crash(String),
    Spawn,
}

impl Cluster {
    fn new(machines: impl Iterator<Item = impl Into<String>>) -> Self {
        Self {
            machines: machines.map(Into::into).collect(),
            events: VecDeque::new(),
        }
    }

    fn start(&mut self, sim: &mut Sim<'_>) {
        for machine in &self.machines {
            sim.host(machine.as_str(), echo_server);
        }
    }
}

#[test]
fn tcp_echo() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();

    let mut sim = Builder::new()
        .simulation_duration(Duration::from_secs(20))
        .build();

    let should_crash = Rc::new(RefCell::new(false));

    sim.host("server", echo_server);

    let mut workloads = CompoundWorkload::new();

    workloads.add_workload(EchoClientCycle {
        clients: 5,
        duration: Duration::from_secs(10),
        addr: "server:8080".into(),
    });

    workloads.add_workload(KillServer {
        when: Duration::from_secs(5),
        restore: Some(Duration::from_secs(8)),
        should_crash: should_crash.clone(),
    });

    workloads.apply(&mut sim);

    loop {
        if sim.step().unwrap() {
            break;
        }

        if *should_crash.borrow() {
            tracing::info!("crashing");
            sim.crash("server");
        }
    }
}

async fn echo_server() -> Result<(), Box<dyn Error>> {
    let addr = "0.0.0.0:8080";

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Listening on: {addr}");

    loop {
        let (socket, _) = listener.accept().await?;

        async fn handler(mut socket: TcpStream) -> Result<(), Box<dyn Error>> {
            let mut buf = vec![0; 1024];

            loop {
                let n = socket.read(&mut buf).await?;

                if n == 0 {
                    return Err("")?;
                }

                socket.write_all(&buf[0..n]).await?;
            }
        }

        tokio::task::spawn_local(async move {
            if let Err(e) = handler(socket).await {
                tracing::error!("handler error: {:?}", e);
            }
        });
    }
}

async fn echo_client(addr: impl AsRef<str>) -> Result<(), Box<dyn Error>> {
    let mut s =
        tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(addr.as_ref())).await??;

    let how_many = 3;
    s.write_u8(how_many).await?;

    // for i in 0..how_many {
    //     assert_eq!(i, s.read_u8().await?);
    // }

    Ok(())
}

workload! {
    struct EchoClientCycle {
        #[config]
        clients: usize,
        #[config]
        duration: Duration,
        #[config]
        addr: String,

        errors: Rc<RefCell<usize>>,
    }

    impl Workload for EchoClientCycle {
        fn name(&self) -> &str {
            "EchoClientCycle"
        }

        async fn setup(&mut self) {}

        async fn run(&mut self) {
            let mut join = JoinSet::new();

            for _ in 0..self.clients {
                let addr = self.addr.clone();
                let errors = self.errors.clone();
                let duration = self.duration;

                join.spawn_local(async move {
                    while turmoil::elapsed() < duration {
                        if let Err(e) = echo_client(&addr).await {
                            println!("echo_client error: {:?}", e);
                            *errors.borrow_mut() += 1;
                        }
                    }
                });
            }

            let _ = join.join_all().await;
        }

        async fn verify(&mut self) {
            let errors = self.errors.borrow();
            assert_eq!(*errors, 0, "echo client's had errors: {}", errors);

            tracing::info!("completed");
        }
    }
}

workload! {
    struct KillServer {
        #[config]
        when: Duration,
        #[config]
        restore: Option<Duration>,
        #[config]
        should_crash: Rc<RefCell<bool>>,
    }

    impl Workload for KillServer {
        fn name(&self) -> &str {
            "KillServer"
        }

        async fn setup(&mut self) {}

        async fn run(&mut self) {
            tokio::time::sleep(self.when).await;

            tracing::info!("assassinating");
            *self.should_crash.borrow_mut() = true;

            if let Some(dur) = self.restore {
                if turmoil::elapsed() >= dur {
                    *self.should_crash.borrow_mut() = false;
                }
            }
        }

        async fn verify(&mut self) {}
    }
}
