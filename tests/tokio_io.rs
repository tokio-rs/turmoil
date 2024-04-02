use tokio::fs::remove_file;
use tokio::net::UnixListener;
use tokio_test::assert_err;

use turmoil::Builder;

/// test assumes IO operation (binding unix domain socket) will succeed
#[test]
fn test_tokio_with_io_enabled() -> turmoil::Result {
    let mut sim = Builder::new().enable_tokio_io().build();
    // client, which would panic if tokio io wouldn't be enabled
    sim.client("client", async move {
        let path = "/tmp/test_socket1";
        // bind unix domain socket -> needs tokio io
        let _ = UnixListener::bind(path).unwrap();
        // remove socket file
        let _ = remove_file(path).await?;
        Ok(())
    });

    sim.run()
}

/// test assumes IO operation (binding unix domain socket) will fail
#[test]
fn test_tokio_with_io_disabled() -> () {
    let mut sim = Builder::new().build();
    // client, which would panic (if not catched) since tokio is not
    sim.client("client", async move {
        let path = "/tmp/test_socket2";
        let result = std::panic::catch_unwind(|| {
            let _ = UnixListener::bind(path);
        });
        assert_err!(result);

        // remove socket file
        tokio::fs::remove_file(path).await?;
        Ok(())
    });

    let _ = sim.run();
}
