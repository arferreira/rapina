//! Integration tests for `Rapina::on_startup()` lifecycle hooks.
//!
//! These are separate-crate tests, so they exercise the public API only.
//! `TestClient` deliberately does not run startup hooks, so the failing-hook
//! cases go through a real `listen()` call and assert on its error. The last
//! test covers the happy path: both hooks run, the listener binds, and the
//! server answers a real request over a socket.
//!
//! Every `listen()` that is expected to return is wrapped in a timeout so a
//! mis-wired implementation fails the test instead of hanging CI forever.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Imported by name rather than glob: `rapina::prelude::*` also exports
// `rapina::Result`, which would shadow `std::result::Result` in this file.
use rapina::http::{Method, StatusCode};
use rapina::prelude::Rapina;
use rapina::router::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Generous upper bound: startup hooks are a handful of bookkeeping steps, so
/// anything approaching this means `listen()` never observed the hook result.
const LISTEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The concrete error type every startup hook must return.
type HookResult = Result<(), Box<dyn Error + Send + Sync>>;

/// A hook that panics synchronously, before it even produces a future.
fn panicking_sync_hook() -> std::future::Ready<HookResult> {
    panic!("sync boom")
}

/// A hook whose future panics after being polled at least once.
async fn panicking_async_hook() -> HookResult {
    tokio::task::yield_now().await;
    panic!("async boom")
}

/// A hook that panics with a payload that is not a `&str`/`String`.
async fn non_string_panic_hook() -> HookResult {
    std::panic::panic_any(42u32)
}

/// Runs `listen()` on `addr` under a timeout and requires it to fail, returning
/// the error so callers can inspect its message.
async fn listen_expect_err_at(app: Rapina, addr: &str) -> std::io::Error {
    let result = tokio::time::timeout(LISTEN_TIMEOUT, app.listen(addr))
        .await
        .expect(
            "listen() did not return within 10s; startup hooks are not being awaited before bind",
        );
    result.expect_err("listen() must fail when a startup hook fails")
}

/// Runs `listen()` on an ephemeral port under a timeout and requires it to fail.
async fn listen_expect_err(app: Rapina) -> std::io::Error {
    listen_expect_err_at(app, "127.0.0.1:0").await
}

/// Binds an ephemeral port, records its address, and releases it again so a
/// hook can prove the server has not bound it yet.
fn free_port_addr() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind a free port");
    let addr = listener
        .local_addr()
        .expect("failed to read the ephemeral address");
    drop(listener);
    addr
}

/// Reserves an ephemeral port and releases it so the spawned server can bind it.
async fn free_port() -> u16 {
    free_port_addr().port()
}

/// Spawns the app on a free port on a separate OS thread with its own
/// runtime. Required because `Rapina: !Sync` means its `listen` future
/// is `!Send` and cannot be `tokio::spawn`-ed.
///
/// The thread is never joined (the same pattern as `streaming.rs`): the
/// server is meant to outlive the test body. Startup is proven by polling the
/// socket, which fails the test after a bounded number of attempts if the app
/// never binds (for example because a hook is not awaited).
async fn spawn_app<F>(build_app: F) -> u16
where
    F: FnOnce() -> Rapina + Send + 'static,
{
    let port = free_port().await;
    let addr = format!("127.0.0.1:{}", port);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build a current-thread runtime");
        rt.block_on(async move {
            let app = build_app();
            let _ = app.listen(&addr).await;
        });
    });
    for _ in 0..100 {
        if TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .is_ok()
        {
            return port;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not start on port {}", port);
}

/// Sends a raw HTTP/1.1 `GET /ping` and returns the response head (everything
/// through, and including, the `\r\n\r\n` header terminator).
async fn get_ping_head(port: u16) -> String {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .expect("failed to connect to the spawned server");
    let request = "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    stream
        .write_all(request.as_bytes())
        .await
        .expect("failed to send the request");

    let mut buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 512];
    loop {
        let n = stream
            .read(&mut tmp)
            .await
            .expect("failed to read response");
        assert!(n > 0, "connection closed before the response headers");
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn startup_hook_error_makes_listen_fail() {
    let app = Rapina::new()
        .disable_introspection()
        .on_startup(|| async { Err(std::io::Error::other("boom").into()) });

    let err = listen_expect_err(app).await;
    let message = err.to_string();

    assert!(
        message.contains("startup hook 1 failed"),
        "error should name the failing hook, got: {message}"
    );
    assert!(
        message.contains("boom"),
        "error should carry the hook's error, got: {message}"
    );
}

#[tokio::test]
async fn startup_hooks_run_in_registration_order() {
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let first = Arc::clone(&log);
    let second = Arc::clone(&log);
    let third = Arc::clone(&log);

    let app = Rapina::new()
        .disable_introspection()
        .on_startup(move || {
            let log = Arc::clone(&first);
            async move {
                log.lock().unwrap().push("first");
                Ok(())
            }
        })
        .on_startup(move || {
            let log = Arc::clone(&second);
            async move {
                log.lock().unwrap().push("second");
                Ok(())
            }
        })
        .on_startup(move || {
            let log = Arc::clone(&third);
            async move {
                log.lock().unwrap().push("third");
                Err(std::io::Error::other("third failed").into())
            }
        });

    let err = listen_expect_err(app).await;
    assert!(
        err.to_string().contains("startup hook 3 failed"),
        "the third hook is the one that failed, got: {err}"
    );

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["first", "second", "third"],
        "hooks must run in registration order"
    );
}

#[tokio::test]
async fn startup_hook_failure_skips_remaining_hooks() {
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let first = Arc::clone(&log);
    let third = Arc::clone(&log);

    let app = Rapina::new()
        .disable_introspection()
        .on_startup(move || {
            let log = Arc::clone(&first);
            async move {
                log.lock().unwrap().push("one");
                Ok(())
            }
        })
        .on_startup(|| async { Err(std::io::Error::other("second failed").into()) })
        .on_startup(move || {
            let log = Arc::clone(&third);
            async move {
                log.lock().unwrap().push("three");
                Ok(())
            }
        });

    let err = listen_expect_err(app).await;
    assert!(
        err.to_string().contains("startup hook 2 failed"),
        "error should point at the second hook, got: {err}"
    );

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["one"],
        "hooks after the first failure must not run"
    );
}

#[tokio::test]
async fn synchronous_panic_in_hook_is_caught() {
    let app = Rapina::new()
        .disable_introspection()
        .on_startup(panicking_sync_hook);

    let err = listen_expect_err(app).await;
    let message = err.to_string();

    assert!(
        message.contains("startup hook 1 panicked"),
        "a synchronous panic must be caught and reported, got: {message}"
    );
    assert!(
        message.contains("sync boom"),
        "the panic payload must be preserved, got: {message}"
    );
}

#[tokio::test]
async fn async_panic_in_hook_is_caught() {
    let app = Rapina::new()
        .disable_introspection()
        .on_startup(panicking_async_hook);

    let err = listen_expect_err(app).await;
    let message = err.to_string();

    assert!(
        message.contains("startup hook 1 panicked"),
        "a panic while polling the hook future must be caught, got: {message}"
    );
    assert!(
        message.contains("async boom"),
        "the panic payload must be preserved, got: {message}"
    );
}

#[tokio::test]
async fn panic_in_second_hook_skips_third() {
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let first = Arc::clone(&log);
    let third = Arc::clone(&log);

    let app = Rapina::new()
        .disable_introspection()
        .on_startup(move || {
            let log = Arc::clone(&first);
            async move {
                log.lock().unwrap().push("one");
                Ok(())
            }
        })
        .on_startup(panicking_async_hook)
        .on_startup(move || {
            let log = Arc::clone(&third);
            async move {
                log.lock().unwrap().push("three");
                Ok(())
            }
        });

    let err = listen_expect_err(app).await;
    assert!(
        err.to_string().contains("startup hook 2 panicked"),
        "the second hook is the one that panicked, got: {err}"
    );

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["one"],
        "hooks after a panicking hook must not run"
    );
}

#[tokio::test]
async fn startup_hooks_run_before_listener_is_bound() {
    // The probe address must be the one `listen()` would bind: if startup hooks
    // ever ran after the listener bound, this hook's bind would fail with
    // EADDRINUSE and the assertion below would catch it.
    let addr = free_port_addr();
    let was_free: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

    let slot = Arc::clone(&was_free);
    let app = Rapina::new().disable_introspection().on_startup(move || {
        let slot = Arc::clone(&slot);
        async move {
            let bound = std::net::TcpListener::bind(addr).is_ok();
            *slot.lock().unwrap() = Some(bound);
            // Stop here so `listen()` returns without ever binding.
            Err(std::io::Error::other("stop before bind").into())
        }
    });

    let err = listen_expect_err_at(app, &addr.to_string()).await;
    assert!(
        err.to_string().contains("startup hook 1 failed"),
        "the hook must fail, not panic, to keep the assertion honest, got: {err}"
    );

    let bound = *was_free.lock().unwrap();
    assert_eq!(
        bound,
        Some(true),
        "the listener must not be bound while startup hooks run"
    );
}

#[tokio::test]
async fn non_string_panic_payload_is_caught() {
    let app = Rapina::new()
        .disable_introspection()
        .on_startup(non_string_panic_hook);

    let err = listen_expect_err(app).await;
    assert!(
        err.to_string().contains("non-string panic payload"),
        "a non-string payload must still be reported, got: {err}"
    );
}

#[tokio::test]
async fn startup_hook_error_preserves_source() {
    let app = Rapina::new()
        .disable_introspection()
        .on_startup(|| async { Err(std::io::Error::other("hook source boom").into()) });

    let err = listen_expect_err(app).await;
    let message = err.to_string();
    assert!(
        message.contains("startup hook 1 failed"),
        "error should name the failing hook, got: {message}"
    );
    assert!(
        message.contains("hook source boom"),
        "error should carry the hook's error, got: {message}"
    );

    let source = err
        .get_ref()
        .expect("io::Error must carry the hook error")
        .source()
        .expect("hook failure must expose its source");
    assert!(
        source.downcast_ref::<std::io::Error>().is_some(),
        "the original hook error must be recoverable by downcast, got: {source}"
    );
}

#[tokio::test]
async fn successful_startup_hooks_serve_requests() {
    let log: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let first = Arc::clone(&log);
    let second = Arc::clone(&log);

    let router = Router::new().route(Method::GET, "/ping", |_req, _params, _state| async {
        StatusCode::OK
    });

    let port = spawn_app(move || {
        Rapina::new()
            .disable_introspection()
            .router(router)
            .on_startup(move || {
                let log = Arc::clone(&first);
                async move {
                    log.lock().unwrap().push("first");
                    Ok(())
                }
            })
            .on_startup(move || {
                let log = Arc::clone(&second);
                async move {
                    log.lock().unwrap().push("second");
                    Ok(())
                }
            })
    })
    .await;

    // Hooks run before the listener binds, so a reachable socket plus a real
    // response proves they completed successfully. The timeout keeps a
    // regression from hanging the test.
    let head = tokio::time::timeout(LISTEN_TIMEOUT, get_ping_head(port))
        .await
        .expect("the spawned server did not answer GET /ping within 10s");

    assert!(
        head.starts_with("HTTP/1.1 200"),
        "expected a 200 response from the live server, got: {head:?}"
    );

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["first", "second"],
        "both hooks must run, in order, and only once before the served request"
    );
}
