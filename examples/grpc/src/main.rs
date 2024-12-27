use connector::connector;
use proto::greeter_client::GreeterClient;
use proto::greeter_server::{Greeter, GreeterServer};
use proto::{HelloReply, HelloRequest};
use std::net::{IpAddr, Ipv4Addr};
use tonic::transport::{Endpoint, Server};
use tonic::Status;
use tonic::{Request, Response};
use tracing::{info_span, Instrument};
use turmoil::net::TcpListener;
use turmoil::Builder;

#[allow(non_snake_case)]
mod proto {
    tonic::include_proto!("helloworld");
}

fn main() {
    configure_tracing();

    let addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);

    let mut sim = Builder::new().build();

    let greeter = GreeterServer::new(MyGreeter {});

    sim.host("server", move || {
        let greeter = greeter.clone();
        async move {
            Server::builder()
                .add_service(greeter)
                .serve_with_incoming(async_stream::stream! {
                    let listener = TcpListener::bind(addr).await?;
                    loop {
                        yield listener.accept().await.map(|(s, _)| incoming::Accepted(s));
                    }
                })
                .await?;

            Ok(())
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let ch = Endpoint::new("http://server:9999")?
                .connect_with_connector(connector())
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
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                    .from_env_lossy(),
            )
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

mod incoming {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tonic::transport::server::{Connected, TcpConnectInfo};
    use turmoil::net::TcpStream;

    pub struct Accepted(pub TcpStream);

    impl Connected for Accepted {
        type ConnectInfo = TcpConnectInfo;

        fn connect_info(&self) -> Self::ConnectInfo {
            Self::ConnectInfo {
                local_addr: self.0.local_addr().ok(),
                remote_addr: self.0.peer_addr().ok(),
            }
        }
    }

    impl AsyncRead for Accepted {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Accepted {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, std::io::Error>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }
}

mod connector {
    use std::{future::Future, pin::Pin};

    use hyper::Uri;
    use hyper_util::rt::TokioIo;

    use tower::Service;
    use turmoil::net::TcpStream;

    type Fut = Pin<Box<dyn Future<Output = Result<TokioIo<TcpStream>, std::io::Error>> + Send>>;

    pub fn connector(
    ) -> impl Service<Uri, Response = TokioIo<TcpStream>, Error = std::io::Error, Future = Fut> + Clone
    {
        tower::service_fn(|uri: Uri| {
            Box::pin(async move {
                let conn = TcpStream::connect(uri.authority().unwrap().as_str()).await?;
                Ok::<_, std::io::Error>(TokioIo::new(conn))
            }) as Fut
        })
    }
}
