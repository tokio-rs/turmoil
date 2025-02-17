use axum::{body::Body, extract::Path, http::Request, routing::get, serve::Listener, Router};
use http_body_util::BodyExt as _;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tracing::{info_span, Instrument};
use turmoil::{net, Builder};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    let addr = (IpAddr::from(Ipv4Addr::UNSPECIFIED), 9999);

    let mut sim = Builder::new().build();

    let router = Router::new().route(
        "/greet/{name}",
        get(|Path(name): Path<String>| async move { format!("Hello {name}!") }),
    );

    sim.host("server", move || {
        let router = router.clone();
        async move {
            let listener = net::TcpListener::bind(addr).await?;
            axum::serve(TurmoilListener(listener), router)
                .await
                .map_err(Into::into)
        }
        .instrument(info_span!("server"))
    });

    sim.client(
        "client",
        async move {
            let client = Client::builder(TokioExecutor::new()).build(connector::connector());

            let mut request = Request::new(Body::empty());
            *request.uri_mut() = hyper::Uri::from_static("http://server:9999/greet/foo");
            let res = client.request(request).await?;

            let (parts, body) = res.into_parts();
            let body = body.collect().await?.to_bytes();
            let res = hyper::Response::from_parts(parts, body);

            tracing::info!("Got response: {:?}", res);

            Ok(())
        }
        .instrument(info_span!("client")),
    );

    sim.run().unwrap();
}

struct TurmoilListener(net::TcpListener);

impl Listener for TurmoilListener {
    type Io = net::TcpStream;

    type Addr = SocketAddr;

    fn accept(&mut self) -> impl std::future::Future<Output = (Self::Io, Self::Addr)> + Send {
        async move { self.0.accept().await.unwrap() }
    }

    fn local_addr(&self) -> tokio::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

mod connector {
    use hyper::Uri;
    use pin_project_lite::pin_project;
    use std::{future::Future, io::Error, pin::Pin};
    use tokio::io::AsyncWrite;
    use tower::Service;
    use turmoil::net::TcpStream;

    type Fut = Pin<Box<dyn Future<Output = Result<TurmoilConnection, Error>> + Send>>;

    pub fn connector(
    ) -> impl Service<Uri, Response = TurmoilConnection, Error = Error, Future = Fut> + Clone {
        tower::service_fn(|uri: Uri| {
            Box::pin(async move {
                let conn = TcpStream::connect(uri.authority().unwrap().as_str()).await?;
                Ok::<_, Error>(TurmoilConnection { fut: conn })
            }) as Fut
        })
    }

    pin_project! {
        pub struct TurmoilConnection{
            #[pin]
            fut: turmoil::net::TcpStream
        }
    }

    impl hyper::rt::Read for TurmoilConnection {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            mut buf: hyper::rt::ReadBufCursor<'_>,
        ) -> std::task::Poll<Result<(), Error>> {
            let n = unsafe {
                let mut tbuf = tokio::io::ReadBuf::uninit(buf.as_mut());
                let result = tokio::io::AsyncRead::poll_read(self.project().fut, cx, &mut tbuf);
                match result {
                    std::task::Poll::Ready(Ok(())) => tbuf.filled().len(),
                    other => return other,
                }
            };

            unsafe {
                buf.advance(n);
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl hyper::rt::Write for TurmoilConnection {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<Result<usize, Error>> {
            Pin::new(&mut self.fut).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Error>> {
            Pin::new(&mut self.fut).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Error>> {
            Pin::new(&mut self.fut).poll_shutdown(cx)
        }
    }

    impl hyper_util::client::legacy::connect::Connection for TurmoilConnection {
        fn connected(&self) -> hyper_util::client::legacy::connect::Connected {
            hyper_util::client::legacy::connect::Connected::new()
        }
    }
}
