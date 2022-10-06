use std::{
    convert::Infallible,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use hyper::{
    client::conn::handshake,
    server::accept::from_stream,
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};
use tokio::task::spawn_local;
use turmoil::{net, Builder};

#[test]
fn smoke() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut sim = Builder::new().build();

    sim.host("server", || {
        let count = count.clone();
        async move {
            let listener = net::TcpListener::bind().await.unwrap();

            let accept = from_stream(async_stream::stream! {
                yield listener.accept().await.map(|(s, _)| s);
            });

            Server::builder(accept)
                .serve(make_service_fn(move |_| {
                    let count = count.clone();
                    async move {
                        Ok::<_, Infallible>(service_fn(move |_: Request<Body>| {
                            let count = count.clone();
                            async move {
                                count.fetch_add(1, Ordering::SeqCst);
                                Ok::<_, Infallible>(Response::new(Body::from("Hello World!")))
                            }
                        }))
                    }
                }))
                .await
                .unwrap();
        }
    });

    let count = count.clone();
    sim.client("client", async move {
        let s = net::TcpStream::connect("server").await.unwrap();

        let (mut sr, conn) = handshake(s).await.unwrap();

        spawn_local(conn);

        let req = Request::new(Body::empty());

        sr.send_request(req).await.unwrap();

        assert_eq!(count.load(Ordering::SeqCst), 1);

        Ok(())
    });

    sim.run().unwrap();
}
