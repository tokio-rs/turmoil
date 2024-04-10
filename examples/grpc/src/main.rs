use hyper::server::accept::from_stream;
use hyper::Server;
use std::net::{IpAddr, Ipv4Addr};
use tonic::transport::Endpoint;
use tonic::Status;
use tonic::{Request, Response};
use tower::make::Shared;
use tracing::{info_span, Instrument};
use turmoil::{net, Builder};

#[allow(non_snake_case)]
mod proto {
    tonic::include_proto!("helloworld");
}

use proto::greeter_server::{Greeter, GreeterServer};
use proto::{HelloReply, HelloRequest};

use crate::proto::greeter_client::GreeterClient;

fn main() {
    configure_tracing();

    let addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);

    let mut sim = Builder::new().build();

    let greeter = GreeterServer::new(MyGreeter {});

    sim.host("server", move || {
        let greeter = greeter.clone();
        async move {
            Server::builder(from_stream(async_stream::stream! {
                let listener = net::TcpListener::bind(addr).await?;
                loop {
                    yield listener.accept().await.map(|(s, _)| s);
                }
            }))
            .serve(Shared::new(greeter))
            .await
            .unwrap();

            Ok(())
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let ch = Endpoint::new("http://server:9999")?
                .connect_with_connector(connector::connector())
                .await?;
            let mut greeter_client = GreeterClient::new(ch);

            let request = Request::new(HelloRequest { name: "foo".into() });
            let res = greeter_client.say_hello(request).await?;

            tracing::info!(?res, "Got response");

            Ok(())
        }
        .instrument(info_span!("client")),
    );

    sim.run().unwrap();
}

/// An example of how to configure a tracing subscriber that will log logical
/// elapsed time since the simulation started using `turmoil::sim_elapsed()`.
fn configure_tracing() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_timer(SimElapsedTime)
            .finish(),
    )
    .expect("Configure tracing");
}

#[derive(Clone)]
struct SimElapsedTime;
impl tracing_subscriber::fmt::time::FormatTime for SimElapsedTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        // Prints real time and sim elapsed time. Example: 2024-01-10T17:06:57.020452Z [76ms]
        tracing_subscriber::fmt::time()
            .format_time(w)
            .and_then(|()| write!(w, " [{:?}]", turmoil::sim_elapsed().unwrap_or_default()))
    }
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
        tracing::info!(?request, "Got request");
        let reply = HelloReply {
            message: format!("Hello {}!", request.into_inner().name),
        };
        Ok(Response::new(reply))
    }
}

mod connector {
    use std::{future::Future, pin::Pin};

    use hyper::{
        client::connect::{Connected, Connection},
        Uri,
    };
    use tokio::io::{AsyncRead, AsyncWrite};
    use tower::Service;
    use turmoil::net::TcpStream;

    type Fut = Pin<Box<dyn Future<Output = Result<TurmoilConnection, std::io::Error>> + Send>>;

    pub fn connector(
    ) -> impl Service<Uri, Response = TurmoilConnection, Error = std::io::Error, Future = Fut> + Clone
    {
        tower::service_fn(|uri: Uri| {
            Box::pin(async move {
                let conn = TcpStream::connect(uri.authority().unwrap().as_str()).await?;
                Ok::<_, std::io::Error>(TurmoilConnection(conn))
            }) as Fut
        })
    }

    pub struct TurmoilConnection(turmoil::net::TcpStream);

    impl AsyncRead for TurmoilConnection {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for TurmoilConnection {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, std::io::Error>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), std::io::Error>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), std::io::Error>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    impl Connection for TurmoilConnection {
        fn connected(&self) -> hyper::client::connect::Connected {
            Connected::new()
        }
    }
}
