use axum::extract::Path;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use axum::{body::Body, http::Request};
use hyper::server::accept::from_stream;
use hyper::{Client, Server, Uri};
use std::net::{IpAddr, Ipv4Addr};
use tower::make::Shared;
use tracing::{info_span, Instrument};
use turmoil::{net, Builder};

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    tracing_subscriber::fmt::init();

    let addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);

    let mut sim = Builder::new().build();

    let router = Router::new().route(
        "/greet/:name",
        get(|Path(name): Path<String>| async move { format!("Hello {name}!") }),
    );

    sim.host("server", move || {
        let router = router.clone();
        async move {
            Server::builder(from_stream(async_stream::stream! {
                let listener = net::TcpListener::bind(addr).await?;
                loop {
                    yield listener.accept().await.map(|(s, _)| s);
                }
            }))
            .serve(Shared::new(router))
            .await
            .unwrap();

            Ok(())
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let client = Client::builder().build(connector::connector());

            let mut request = Request::new(Body::empty());
            *request.uri_mut() = Uri::from_static("http://server:9999/greet/foo");
            let res = client.request(request).await?;

            let (parts, body) = res.into_parts();
            let body = hyper::body::to_bytes(body).await?;
            let res = Response::from_parts(parts, body);

            tracing::info!("Got response: {:?}", res);

            Ok(())
        }
        .instrument(info_span!("client")),
    );

    sim.run().unwrap();
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
