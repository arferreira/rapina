use std::future::Future;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::time::Duration;

use hyper::Request;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;

use crate::date_cache::DateHeaderCache;
use crate::middleware::MiddlewareStack;
use crate::router::Router;
use crate::state::AppState;

/// A shutdown hook: a closure that returns a boxed future.
pub(crate) type ShutdownHook = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Start the HTTP server and block until a shutdown signal is received.
///
/// Binds to `addr`, runs `router` behind `middlewares`, and performs a graceful
/// shutdown on SIGINT or SIGTERM, waiting up to `shutdown_timeout` for
/// in-flight requests to drain before forcing a close. After all connections
/// are drained, each `shutdown_hooks` closure is awaited in registration order.
pub(crate) async fn serve(
    mut router: Router,
    state: AppState,
    middlewares: MiddlewareStack,
    addr: SocketAddr,
    shutdown_timeout: Duration,
    shutdown_hooks: Vec<ShutdownHook>,
) -> std::io::Result<()> {
    router.freeze();
    let router = Arc::new(router);
    let state = Arc::new(state);
    let middlewares = Arc::new(middlewares);
    let date_cache = DateHeaderCache::start();
    let listener = TcpListener::bind(addr).await?;
    let graceful = GracefulShutdown::new();
    let mut ctrl_c = pin!(tokio::signal::ctrl_c());

    let mut sigterm: Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::SignalKind;
            tokio::signal::unix::signal(SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        }
        #[cfg(not(unix))]
        {
            std::future::pending::<()>().await;
        }
    });

    tracing::info!("Rapina listening on http://{}", addr);

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result?;
                let io = TokioIo::new(stream);
                let router = router.clone();
                let state = state.clone();
                let middlewares = middlewares.clone();

                let date_cache = date_cache.clone();
                let service = service_fn(move |mut req: Request<Incoming>| {
                    let router = router.clone();
                    let state = state.clone();
                    let middlewares = middlewares.clone();
                    let date_cache = date_cache.clone();

                    let ctx = router.prepare_request(&mut req);

                    async move {
                        let mut response = middlewares.execute(req, router, state, &ctx).await;
                        response
                            .headers_mut()
                            .insert(http::header::DATE, date_cache.header_value());
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });

                let conn = auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, service)
                    .into_owned();
                let conn = graceful.watch(conn);

                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::error!("connection error: {}", e);
                    }
                });
            }
            _ = ctrl_c.as_mut() => {
                drop(listener);
                tracing::info!("Shutdown signal received, waiting for connections to drain...");
                break;
            }

            _ = sigterm.as_mut()  => {
                drop(listener);
                tracing::info!("Shutdown signal received, waiting for connections to drain...");
                break;
            }
        }
    }

    tokio::select! {
        _ = graceful.shutdown() => {
            tracing::info!("All connections drained.");
        }
        _ = tokio::time::sleep(shutdown_timeout) => {
            tracing::warn!("Shutdown timeout reached, forcing close.");
        }
    }

    for hook in shutdown_hooks {
        hook().await;
    }

    tracing::info!("Server stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use serial_test::serial;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn free_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }

    async fn http_get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }

    #[cfg(unix)]
    mod unix_tests {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::getpid;

        pub(super) fn send_sigint() {
            kill(getpid(), Signal::SIGINT).unwrap();
        }

        pub(super) fn send_sigterm() {
            kill(getpid(), Signal::SIGTERM).unwrap();
        }
    }

    #[cfg(windows)]
    mod windows_tests {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};

        pub(super) fn send_ctrl_break() {
            unsafe {
                GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0);
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_shutdown_hooks_execute_in_order() {
        let port = free_port().await;
        let log = Arc::new(Mutex::new(Vec::<String>::new()));

        let log1 = log.clone();
        let log2 = log.clone();

        let router = Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" });

        let handle = tokio::spawn(serve(
            router,
            AppState::new(),
            MiddlewareStack::new(),
            format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_secs(5),
            vec![
                Box::new(move || {
                    Box::pin(async move {
                        log1.lock().unwrap().push("db_pool_closed".to_string());
                    }) as Pin<Box<dyn Future<Output = ()> + Send>>
                }),
                Box::new(move || {
                    Box::pin(async move {
                        log2.lock().unwrap().push("metrics_flushed".to_string());
                    }) as Pin<Box<dyn Future<Output = ()> + Send>>
                }),
            ],
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;

        let response = http_get(port, "/").await;
        assert!(response.contains("200"), "server should respond with 200");

        #[cfg(unix)]
        unix_tests::send_sigint();

        #[cfg(windows)]
        windows_tests::send_ctrl_break();

        let result = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(result.is_ok(), "server should shut down within timeout");
        assert!(
            result.unwrap().unwrap().is_ok(),
            "server should exit cleanly"
        );

        let entries = log.lock().unwrap();
        assert_eq!(
            *entries,
            vec!["db_pool_closed", "metrics_flushed"],
            "shutdown hooks should run in registration order"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_inflight_request_completes_before_shutdown() {
        let port = free_port().await;

        let router = Router::new().route(http::Method::GET, "/slow", |_, _, _| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            "done"
        });

        let handle = tokio::spawn(serve(
            router,
            AppState::new(),
            MiddlewareStack::new(),
            format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_secs(5),
            vec![],
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;

        let response_task = tokio::spawn(async move { http_get(port, "/slow").await });

        tokio::time::sleep(Duration::from_millis(50)).await;

        #[cfg(unix)]
        unix_tests::send_sigint();

        #[cfg(windows)]
        windows_tests::send_ctrl_break();

        let response = tokio::time::timeout(Duration::from_secs(5), response_task)
            .await
            .expect("response should arrive within timeout")
            .expect("response task should not panic");

        assert!(
            response.contains("done"),
            "in-flight request should complete during graceful shutdown"
        );

        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    #[serial]
    async fn test_shutdown_timeout_enforced() {
        let port = free_port().await;

        let router = Router::new().route(http::Method::GET, "/hang", |_, _, _| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            "never"
        });

        let handle = tokio::spawn(serve(
            router,
            AppState::new(),
            MiddlewareStack::new(),
            format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_secs(1),
            vec![],
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;

        let _hang = tokio::spawn(async move {
            let _ = http_get(port, "/hang").await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        #[cfg(unix)]
        unix_tests::send_sigint();

        #[cfg(windows)]
        windows_tests::send_ctrl_break();

        let result = tokio::time::timeout(Duration::from_secs(3), handle).await;
        assert!(
            result.is_ok(),
            "server should exit after shutdown timeout, not wait for hanging connections"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_sigterm_triggers_shutdown() {
        let port = free_port().await;

        let router = Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" });

        let handle = tokio::spawn(serve(
            router,
            AppState::new(),
            MiddlewareStack::new(),
            format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_secs(5),
            vec![],
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;

        let response = http_get(port, "/").await;
        assert!(response.contains("200"), "server should respond with 200");

        #[cfg(unix)]
        unix_tests::send_sigterm();

        #[cfg(windows)]
        windows_tests::send_ctrl_break();

        let result = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(result.is_ok(), "server should shut down within timeout");
        assert!(
            result.unwrap().unwrap().is_ok(),
            "server should exit cleanly after SIGTERM"
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_date_header_present_on_response() {
        let port = free_port().await;

        let router = Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" });

        let handle = tokio::spawn(serve(
            router,
            AppState::new(),
            MiddlewareStack::new(),
            format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_secs(5),
            vec![],
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;

        let response = http_get(port, "/").await;
        assert!(response.contains("200"), "server should respond with 200");
        assert!(
            response.to_lowercase().contains("date:"),
            "response should contain a Date header, got: {}",
            response
        );
        assert!(
            response.contains("GMT"),
            "Date header should contain GMT timezone, got: {}",
            response
        );

        #[cfg(unix)]
        unix_tests::send_sigint();

        #[cfg(windows)]
        windows_tests::send_ctrl_break();

        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }
}
