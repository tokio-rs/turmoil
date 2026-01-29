use std::{
    cell::{RefCell, RefMut},
    collections::VecDeque,
    error::Error,
    future::Future,
    rc::Rc,
    time::Duration,
};

use rand::{rngs::SmallRng, seq::IndexedRandom, Rng, SeedableRng};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};
use turmoil::{
    net::{TcpListener, TcpStream},
    Sim,
};

#[derive(Clone)]
pub struct Cluster {
    shared: Rc<RefCell<State>>,
    rng: Rc<RefCell<SmallRng>>,
}

pub struct State {
    machines: Vec<Machine>,
    events: VecDeque<Event>,
}

pub enum Event {
    Crash(String),
    Spawn,
}

#[derive(Clone, Debug)]
pub struct Machine {
    pub name: String,
    pub failed: bool,
}

impl Cluster {
    pub fn new(servers: usize, seed: Option<u64>) -> Self {
        let machines = (0..servers)
            .map(|i| format!("server_{}", i))
            .map(|name| Machine {
                name,
                failed: false,
            })
            .collect();

        let seed = seed.unwrap_or_else(|| getrandom::u64().unwrap());

        tracing::info!("starting turmoil sim with seed: {}", seed);

        let rng = Rc::new(RefCell::new(SmallRng::seed_from_u64(seed)));

        let state = State {
            machines,
            events: VecDeque::new(),
        };

        Cluster {
            shared: Rc::new(RefCell::new(state)),
            rng,
        }
    }

    pub fn setup(&self, sim: &mut Sim<'_>) {
        let me = self.shared.borrow();

        for machine in me.machines.iter() {
            let machine = machine.clone();
            let name = machine.name.clone();
            sim.host(name.as_str(), move || async move {
                if let Err(e) = server("0.0.0.0:8080".into()).await {
                    tracing::error!("server error: {:?}", e);
                }

                Ok(())
            });
        }
    }

    pub fn get_machines(&self) -> Vec<Machine> {
        self.shared.borrow().machines.clone()
    }

    pub fn crash(&self, machine: String) {
        self.shared
            .borrow_mut()
            .events
            .push_back(Event::Crash(machine));
    }

    pub fn rng(&self) -> RefMut<'_, SmallRng> {
        self.rng.borrow_mut()
    }

    pub fn run(&mut self, sim: &mut Sim<'_>) -> Result<(), Box<dyn Error>> {
        loop {
            for event in self.shared.borrow_mut().events.drain(..) {
                match event {
                    Event::Crash(machine) => sim.crash(machine),
                    Event::Spawn => todo!(),
                }
            }

            if sim.step()? {
                return Ok(());
            }
        }
    }
}

pub fn echo_client_cycle(
    cluster: Cluster,
    clients: usize,
    duration: Duration,
) -> impl Future<Output = Result<(), Box<dyn Error>>> {
    async move {
        let mut join = JoinSet::new();

        for _client in 0..clients {
            let cluster = cluster.clone();

            join.spawn_local(async move {
                let mut client = Client::new(cluster);

                while turmoil::elapsed() < duration {
                    if let Err(e) = client.request().await {
                        client.reset();
                        tracing::debug!("request error: {:?}", e)
                    }
                }
            });
        }

        let _ = join.join_all().await;

        Ok(())
    }
}

pub fn machine_attrition(
    cluster: Cluster,
    duration: Duration,
    machines_to_kill: usize,
    machines_to_leave: usize,
) -> impl Future<Output = Result<(), Box<dyn Error>>> {
    async move {
        tracing::info!("starting attrition workload");
        let mut killed_machines = 0;

        let mean_delay = duration / machines_to_kill as u32;

        loop {
            if turmoil::elapsed() > duration {
                break;
            }

            let jitter = cluster.rng().random_range(0..mean_delay.as_millis());
            tokio::time::sleep(Duration::from_millis(jitter as u64)).await;

            let machines = cluster.get_machines();

            let alive_machines = machines.iter().filter(|m| !m.failed).collect::<Vec<_>>();

            if killed_machines < machines_to_kill && alive_machines.len() > machines_to_leave {
                // Kill random machine
                let machine_to_kill = alive_machines.choose(&mut *cluster.rng()).unwrap();

                tracing::info!("assassinating: {}", machine_to_kill.name);

                cluster.crash(machine_to_kill.name.clone());
                killed_machines += 1;
            }
        }

        Ok(())
    }
}

async fn server(addr: String) -> Result<(), Box<dyn Error>> {
    // Next up we create a TCP listener which will listen for incoming
    // connections. This TCP listener is bound to the address we determined
    // above and must be associated with an event loop.
    let listener = TcpListener::bind(&addr).await?;
    tracing::debug!("Listening on: {addr}");

    loop {
        // Asynchronously wait for an inbound socket.
        let (mut socket, _) = listener.accept().await?;

        tokio::spawn(async move {
            let mut buf = vec![0; 1024];

            // In a loop, read data from the socket and write the data back.
            loop {
                let Ok(n) = socket.read(&mut buf).await else {
                    tracing::error!("failed to read data from socket");
                    return;
                };

                if n == 0 {
                    return;
                }

                let Ok(_) = socket.write_all(&buf[0..n]).await else {
                    tracing::error!("failed to write data to socket");
                    return;
                };
            }
        });
    }
}

struct Client {
    cluster: Cluster,
    conn: Option<TcpStream>,
}

impl Client {
    fn new(cluster: Cluster) -> Self {
        Self {
            cluster,
            conn: None,
        }
    }

    async fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        let mut attempt = 0;
        let init_backoff = 50;

        let machines = self.cluster.get_machines();

        loop {
            let machine = machines.choose(&mut *self.cluster.rng()).unwrap();

            tracing::debug!("connecting to {}", machine.name);

            let addr = format!("{}:8080", machine.name);

            match tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(&addr)).await {
                Ok(Ok(stream)) => {
                    self.conn = Some(stream);
                    return Ok(());
                }
                Ok(Err(e)) => {
                    tracing::debug!("connect error: {:?}", e);
                }
                Err(_) => {
                    tracing::debug!("connect timeout");
                }
            };

            attempt += 1;

            let backoff = std::cmp::min(init_backoff * 2u64.pow(attempt), 3600);
            let jitter = self.cluster.rng().random_range(0..backoff);

            tokio::time::sleep(Duration::from_millis(jitter)).await;
        }
    }

    async fn request(&mut self) -> Result<(), Box<dyn Error>> {
        if self.conn.is_none() {
            self.connect()
                .await
                .map_err(|e| format!("connect: {}", e))?;
        }

        let stream = self.conn.as_mut().unwrap();

        let msg = b"hello world";
        stream
            .write_all(msg)
            .await
            .map_err(|e| format!("write: {}", e))?;

        let mut buf = vec![0; msg.len()];
        let _n = stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| format!("read: {}", e))?;
        assert_eq!(buf, msg);

        Ok(())
    }

    fn reset(&mut self) {
        self.conn = None;
    }
}
