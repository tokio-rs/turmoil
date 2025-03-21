use std::{cell::RefCell, error::Error, rc::Rc};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use turmoil::{
    net::{TcpListener, TcpStream},
    workload,
    workload::CompoundWorkload,
    Builder,
};

#[test]
fn tcp_echo() {
    let mut sim = Builder::new().build();

    sim.host("server", echo_server);

    let mut workloads = CompoundWorkload::new();

    workloads.add_workload(EchoClientCycle {
        clients: 50,
        requests: 1000,
        addr: "server:8080".into(),
    });

    workloads.apply(&mut sim);

    sim.run().unwrap();
}

async fn echo_server() -> Result<(), Box<dyn Error>> {
    let addr = "0.0.0.0:8080";

    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Listening on: {addr}");

    loop {
        let (mut socket, _) = listener.accept().await?;

        tokio::spawn(async move {
            let mut buf = vec![0; 1024];

            // In a loop, read data from the socket and write the data back.
            loop {
                let n = socket
                    .read(&mut buf)
                    .await
                    .expect("failed to read data from socket");

                if n == 0 {
                    return;
                }

                socket
                    .write_all(&buf[0..n])
                    .await
                    .expect("failed to write data to socket");
            }
        });
    }
}

async fn echo_client(addr: impl AsRef<str>) -> Result<(), Box<dyn Error>> {
    let mut s = TcpStream::connect(addr.as_ref()).await?;

    let how_many = 3;
    s.write_u8(how_many).await?;

    for i in 0..how_many {
        assert_eq!(i, s.read_u8().await?);
    }

    Ok(())
}

workload! {
    struct EchoClientCycle {
        #[config]
        clients: usize,
        #[config]
        requests: usize,
        #[config]
        addr: String,

        errors: Rc<RefCell<usize>>,
    }

    impl Workload for EchoClientCycle {
        async fn setup(&mut self) {}

        async fn run(&mut self, sim: &mut Sim<'_>) {
            for _ in 0..self.clients {
                let addr = self.addr.clone();
                let errors = self.errors.clone();
                let requests = self.requests;

                tokio::task::spawn_local(async move {
                    for _ in 0..requests {
                        if let Err(e) = echo_client(&addr).await {
                            tracing::error!("echo_client error: {:?}", e);
                            *errors.borrow_mut() += 1;
                        }
                    }
                });
            }
        }

        async fn verify(&mut self) {
            let errors = self.errors.borrow();
            assert_eq!(*errors, 0, "echo client's had errors: {}", errors);
        }
    }
}

workload! {
    struct KillServer {}

    impl Workload for KillServer {
        async fn setup(&mut self) {}

        async fn run(&mut self, sim: &mut Sim<'_>) {
            tokio::time::sleep(std::time::Duration::from_secs(500)).await;
            // turmoil::
            // find a way to crash the machine here
        }

        async fn verify(&mut self) {}
    }
}
