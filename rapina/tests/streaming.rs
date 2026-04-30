//! Real-socket integration tests for streaming responses.
//!
//! Tower's `oneshot` doesn't exercise chunked transfer or socket-level
//! buffering, so SSE and `StreamResponse` get a real `TcpListener`.
//! Assertions favor behavior (incremental delivery via timing) over
//! protocol implementation details (chunked transfer headers).

use std::time::{Duration, Instant};

use bytes::Bytes;
use rapina::http::Method;
use rapina::middleware::CompressionConfig;
use rapina::prelude::*;
use rapina::response::{BodyError, SseEvent, SseResponse, StreamResponse};
use rapina::router::Router;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Spawns the app on a free port on a separate OS thread with its own
/// runtime. Required because `Rapina: !Sync` means its `listen` future
/// is `!Send` and cannot be `tokio::spawn`-ed.
async fn spawn<F>(build_app: F) -> u16
where
    F: FnOnce() -> Rapina + Send + 'static,
{
    let port = free_port().await;
    let addr = format!("127.0.0.1:{}", port);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let app = build_app();
            let _ = app.listen(&addr).await;
        });
    });
    for _ in 0..50 {
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

/// Send a raw HTTP/1.1 GET. Returns (stream, headers, body_prefix, sent_at).
/// `body_prefix` contains any body bytes that arrived in the same read as the
/// header terminator and would otherwise be lost.
async fn send_request(
    port: u16,
    path: &str,
    extra_headers: &str,
) -> (TcpStream, Vec<u8>, Vec<u8>, Instant) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port))
        .await
        .unwrap();
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}\r\n",
        path, extra_headers
    );
    let sent_at = Instant::now();
    stream.write_all(req.as_bytes()).await.unwrap();
    let raw = read_until_double_crlf(&mut stream).await;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator")
        + 4;
    let headers = raw[..split].to_vec();
    let body_prefix = raw[split..].to_vec();
    (stream, headers, body_prefix, sent_at)
}

/// Reads from the socket until it sees the `\r\n\r\n` header terminator.
async fn read_until_double_crlf(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 256];
    loop {
        let n = stream.read(&mut tmp).await.unwrap();
        assert!(n > 0, "connection closed before headers");
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return buf;
        }
    }
}

#[tokio::test]
async fn test_sse_emits_events_incrementally() {
    let router = Router::new().route(Method::GET, "/events", |_, _, _| async {
        let s = async_stream::stream! {
            for i in 0..3u32 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                yield Ok::<_, BodyError>(SseEvent::data(format!("event-{}", i)));
            }
        };
        SseResponse::new(s).keep_alive(None)
    });

    let port = spawn(|| Rapina::new().with_introspection(false).router(router)).await;

    let (mut stream, headers, _body_prefix, sent_at) =
        send_request(port, "/events", "Connection: close\r\n").await;

    let header_str = String::from_utf8_lossy(&headers);
    assert!(
        header_str.contains("text/event-stream"),
        "missing SSE content-type: {}",
        header_str
    );

    // Read the first event payload from the body. With chunked transfer
    // there's a hex length line first; we only care that some bytes containing
    // "event-0" arrive before the handler finishes (~300 ms).
    let mut body = Vec::with_capacity(256);
    let mut buf = [0u8; 256];
    let first_event_seen_at = loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            panic!("connection closed before first event");
        }
        body.extend_from_slice(&buf[..n]);
        if body.windows(7).any(|w| w == b"event-0") {
            break Instant::now();
        }
    };

    let elapsed = first_event_seen_at.duration_since(sent_at);
    assert!(
        elapsed < Duration::from_millis(250),
        "first event took {:?}, handler should not buffer the stream",
        elapsed
    );
}

#[tokio::test]
async fn test_stream_response_chunks_bytes() {
    let router = Router::new().route(Method::GET, "/chunks", |_, _, _| async {
        let s = async_stream::stream! {
            for i in 0..3u32 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                yield Ok::<_, BodyError>(Bytes::from(format!("chunk-{} ", i)));
            }
        };
        StreamResponse::new(s).content_type("text/plain")
    });

    let port = spawn(|| Rapina::new().with_introspection(false).router(router)).await;

    let (mut stream, _headers, _body_prefix, sent_at) =
        send_request(port, "/chunks", "Connection: close\r\n").await;

    let mut body = Vec::with_capacity(256);
    let mut buf = [0u8; 256];
    let first_chunk_seen_at = loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            panic!("connection closed before first chunk");
        }
        body.extend_from_slice(&buf[..n]);
        if body.windows(7).any(|w| w == b"chunk-0") {
            break Instant::now();
        }
    };

    assert!(
        first_chunk_seen_at.duration_since(sent_at) < Duration::from_millis(250),
        "first chunk should arrive before handler finishes"
    );

    // Drain the rest, then verify all three chunks arrived.
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut body)).await;
    let body_str = String::from_utf8_lossy(&body);
    for i in 0..3 {
        assert!(
            body_str.contains(&format!("chunk-{}", i)),
            "missing chunk-{} in body: {:?}",
            i,
            body_str
        );
    }
}

#[tokio::test]
async fn test_compression_skips_sse_response() {
    let router = Router::new().route(Method::GET, "/events", |_, _, _| async {
        let s = async_stream::stream! {
            for i in 0..3u32 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                yield Ok::<_, BodyError>(SseEvent::data(format!("event-{}", i)));
            }
        };
        SseResponse::new(s).keep_alive(None)
    });

    let port = spawn(|| {
        Rapina::new()
            .with_introspection(false)
            .with_compression(CompressionConfig::default())
            .router(router)
    })
    .await;

    let (mut stream, headers, _body_prefix, sent_at) = send_request(
        port,
        "/events",
        "Accept-Encoding: gzip\r\nConnection: close\r\n",
    )
    .await;

    let header_str = String::from_utf8_lossy(&headers);
    assert!(
        !header_str.to_lowercase().contains("content-encoding"),
        "compression must skip SSE responses: {}",
        header_str
    );
    assert!(header_str.contains("text/event-stream"));

    // And events still arrive incrementally, proves compression didn't wedge
    // the stream by trying to buffer it.
    let mut body = Vec::with_capacity(256);
    let mut buf = [0u8; 256];
    let first_event_seen_at = loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            panic!("connection closed before first event");
        }
        body.extend_from_slice(&buf[..n]);
        if body.windows(7).any(|w| w == b"event-0") {
            break Instant::now();
        }
    };

    assert!(
        first_event_seen_at.duration_since(sent_at) < Duration::from_millis(250),
        "compression in front of SSE wedged the stream"
    );
}

#[tokio::test]
async fn test_compression_streams_large_body_per_chunk() {
    // Producer emits 10 chunks of 4 KiB each, 50 ms apart. With compression,
    // we should see compressed bytes arriving incrementally (not buffered),
    // and the gunzipped result must equal the original.
    let router = Router::new().route(Method::GET, "/big", |_, _, _| async {
        let s = async_stream::stream! {
            for i in 0..10u32 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let mut chunk = vec![b'x'; 4096];
                chunk[0] = b'0' + (i % 10) as u8;
                yield Ok::<_, BodyError>(Bytes::from(chunk));
            }
        };
        StreamResponse::new(s).content_type("text/plain")
    });

    let port = spawn(|| {
        Rapina::new()
            .with_introspection(false)
            .with_compression(CompressionConfig::default())
            .router(router)
    })
    .await;

    let (mut stream, headers, body_prefix, sent_at) = send_request(
        port,
        "/big",
        "Accept-Encoding: gzip\r\nConnection: close\r\n",
    )
    .await;

    let header_str = String::from_utf8_lossy(&headers);
    assert!(
        header_str.to_lowercase().contains("content-encoding: gzip"),
        "expected gzip content-encoding for streaming body: {}",
        header_str
    );
    assert!(
        !header_str.to_lowercase().contains("content-length"),
        "streaming compression must not emit content-length: {}",
        header_str
    );

    // The body_prefix may already contain the first compressed bytes that
    // arrived alongside the headers. If not, read more and time-check.
    let mut all = body_prefix;
    let first_byte_at = if all.is_empty() {
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0, "no bytes received");
        all.extend_from_slice(&buf[..n]);
        Instant::now()
    } else {
        sent_at
    };
    assert!(
        first_byte_at.duration_since(sent_at) < Duration::from_millis(250),
        "first compressed byte took {:?}, compression should not buffer the stream",
        first_byte_at.duration_since(sent_at)
    );

    // Drain the rest, dechunk, and gunzip. Verify roundtrip.
    let _ = tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut all)).await;
    let decoded = decode_chunked(&all);
    eprintln!(
        "raw={} dechunked_len={}\nfirst 200 raw bytes (hex): {}\nlast 100 raw bytes (hex): {}",
        all.len(),
        decoded.len(),
        all.iter()
            .take(200)
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
        all.iter()
            .rev()
            .take(100)
            .rev()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
    );
    let mut gz = flate2::read::GzDecoder::new(&decoded[..]);
    let mut original = Vec::new();
    std::io::Read::read_to_end(&mut gz, &mut original).expect("gunzip roundtrip");
    assert_eq!(original.len(), 4096 * 10);
    assert_eq!(&original[..1], b"0");
    assert_eq!(&original[4096..4097], b"1");
}

/// Minimal HTTP/1.1 chunked-transfer decoder for the test.
fn decode_chunked(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let line_end = match input[i..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => i + p,
            None => break,
        };
        let size_str = std::str::from_utf8(&input[i..line_end]).unwrap_or("0");
        let size = usize::from_str_radix(size_str.trim(), 16).unwrap_or(0);
        i = line_end + 2;
        if size == 0 {
            break;
        }
        if i + size > input.len() {
            break;
        }
        out.extend_from_slice(&input[i..i + size]);
        i += size + 2; // chunk + trailing CRLF
    }
    out
}
