use hyper::server::accept::from_stream;
use hyper::service::make_service_fn;
use hyper::{Client, Server, Uri};
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr};
use tonic::Status;
use tonic::{Request, Response};
use tracing::{info_span, Instrument, Span};
use turmoil::{net, Builder};

mod proto {
    tonic::include_proto!("helloworld");
}

use proto::greeter_server::{Greeter, GreeterServer};
use proto::{HelloReply, HelloRequest};

use crate::proto::greeter_client::GreeterClient;

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt::init();

    let addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);

    let mut sim = Builder::new().build();

    let greeter = GreeterServer::new(MyGreeter {});

    sim.host("server", move || {
        let greeter = greeter.clone();
        async move {
            let listener = net::TcpListener::bind(addr).await?;

            let accept = from_stream(async_stream::stream! {
                loop {
                    yield listener.accept().await.map(|(s, _)| s);
                }
            });

            Server::builder(accept)
                .serve(make_service_fn(move |_| {
                    let greeter = greeter.clone();
                    async move { Ok::<_, Infallible>(greeter) }.instrument(Span::current())
                }))
                .await
                .unwrap();

            Ok(())
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let svc = Client::builder()
                .http2_only(true)
                .build(connector::connector());

            let uri = Uri::from_static("http://server:9999");
            let mut greeter_client = GreeterClient::with_origin(svc, uri);

            let request = Request::new(HelloRequest { name: "foo".into() });

            let res = greeter_client.say_hello(request).await?;

            tracing::info!("Got response: {:?}", res);

            Ok(())
        }
        .instrument(info_span!("client")),
    );

    sim.run().unwrap();
}

#[derive(Default)]
pub struct MyGreeter {}

#[tonic::async_trait]
impl Greeter for MyGreeter {
    async fn say_hello(
        &self,
        request: Request<HelloRequest>,
    ) -> Result<Response<HelloReply>, Status> {
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
