+++
title = "Streaming & SSE"
description = "Streaming response bodies, Server-Sent Events, keep-alive, and compression interaction"
weight = 12
date = 2026-04-26
+++

Rapina supports two response shapes for incremental delivery: `StreamResponse` for arbitrary chunked bytes (file downloads, LLM token streams, custom protocols) and `SseResponse` for [Server-Sent Events](https://html.spec.whatwg.org/multipage/server-sent-events.html). Both write to the wire as data arrives instead of buffering the entire response in memory.

Both ship in the default feature set and live under `rapina::response`.

## Producing the stream

The examples below use [`async-stream`](https://docs.rs/async-stream) to build the `Stream<Item = ...>` argument that `StreamResponse::new` and `SseResponse::new` accept. It's the most ergonomic option, but it's not a Rapina dependency. Add it to your own `Cargo.toml`:

```toml
[dependencies]
async-stream = "0.3"
```

Other ways to produce a stream:
- [`tokio_stream::wrappers::ReceiverStream`](https://docs.rs/tokio-stream/latest/tokio_stream/wrappers/struct.ReceiverStream.html) when you have a `tokio::sync::mpsc::Receiver` already.
- [`futures::stream::iter`](https://docs.rs/futures/latest/futures/stream/fn.iter.html) for a one-shot fixed-size sequence.
- A manual `impl Stream for MyType` when you need precise control over polling.

## Server-Sent Events

`SseResponse` wraps a [`Stream`](https://docs.rs/futures-core/latest/futures_core/stream/trait.Stream.html) of `SseEvent` values and serializes them to the SSE wire format. It sets `Content-Type: text/event-stream`, `Cache-Control: no-cache`, and `X-Accel-Buffering: no` so reverse proxies (nginx, Cloudflare) do not buffer the stream.

```rust
use std::time::Duration;
use rapina::http::Method;
use rapina::prelude::*;
use rapina::response::{BodyError, SseEvent, SseResponse};
use rapina::router::Router;

let router = Router::new().route(Method::GET, "/events", |_, _, _| async {
    let stream = async_stream::stream! {
        for i in 0u64.. {
            tokio::time::sleep(Duration::from_secs(1)).await;
            yield Ok::<_, BodyError>(SseEvent::data(format!("tick {}", i)).id(i.to_string()));
        }
    };
    SseResponse::new(stream)
});
```

`SseEvent` is a small builder. Multi-line `data` produces multiple `data:` lines per the spec.

```rust
SseEvent::data("hello")
    .event("update")
    .id("42")
    .retry(5000);

// Comment-only event (browsers ignore comments). Useful for inline annotations
// or operator-visible markers without affecting the EventSource client.
SseEvent::comment("operator marker");

// Comment alongside data.
SseEvent::data("payload").with_comment("debug-info");
```

### Convenience constructors

For the common cases of "I have a finite list" or "I want to send from a channel", you don't need `async-stream`:

```rust
// Finite list of events.
SseResponse::from_events(vec![
    SseEvent::data("a"),
    SseEvent::data("b"),
]);

// Send from a tokio channel.
let (tx, rx) = tokio::sync::mpsc::channel(32);
tokio::spawn(async move {
    tx.send(Ok(SseEvent::data("via channel"))).await.unwrap();
});
SseResponse::from_receiver(rx);
```

Same `from_receiver` constructor exists on `StreamResponse` for raw `Bytes`.

### Custom headers and status

```rust
SseResponse::new(stream)
    .status(StatusCode::OK)
    .header("x-trace-id", trace_id)
    .keep_alive_text("ping");          // emits `: ping\n\n` instead of the default `:\n\n`

StreamResponse::new(stream)
    .content_type("application/pdf")
    .header("content-disposition", "attachment; filename=\"report.pdf\"");
```

### Keep-alive

Default: a `:\n\n` comment frame every 15 seconds when the user stream is idle. This stops idle proxies from closing the connection. Disable or change the interval:

```rust
SseResponse::new(stream).keep_alive(None);                                  // off
SseResponse::new(stream).keep_alive(Some(Duration::from_secs(30)));         // 30s
```

The keep-alive timer is implemented inside the body itself (no separate task). When the response is dropped, both the user stream and the timer are dropped together, so client disconnect cancels everything cleanly.

### Browser client

Standard EventSource:

```javascript
const es = new EventSource('/events');
es.onmessage = (m) => console.log(m.data);
es.addEventListener('update', (m) => console.log('update:', m.data));
```

## Raw chunked streaming

`StreamResponse` is the lower-level primitive. Wraps any `Stream<Item = Result<Bytes, BodyError>>`. Use it for binary streams, file downloads, or any chunked protocol where you don't want SSE framing.

```rust
use bytes::Bytes;
use rapina::response::{BodyError, StreamResponse};

let router = Router::new().route(Method::GET, "/download", |_, _, _| async {
    let s = async_stream::stream! {
        let file = tokio::fs::File::open("/tmp/big.bin").await.unwrap();
        let mut reader = tokio::io::BufReader::new(file);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await.unwrap();
            if n == 0 { break; }
            yield Ok::<_, BodyError>(Bytes::copy_from_slice(&buf[..n]));
        }
    };
    StreamResponse::new(s).content_type("application/octet-stream")
});
```

`status()` and `content_type()` are chainable. Default status is `200 OK`, default content-type is `application/octet-stream`.

## Compression interaction

The compression middleware does the right thing automatically:

- **`text/event-stream`**: never compressed. Gzip across SSE event boundaries breaks framing at proxies; Starlette ships the same exclusion.
- **Other streaming bodies** (`size_hint().exact() == None`): compressed per-chunk with a persistent gzip/deflate encoder. The `min_size` and "compression not worth it" guards don't apply, since the total size isn't known in advance. Streaming bodies always get compressed when the client accepts it. `Content-Length` is dropped, `Content-Encoding` and `Vary: Accept-Encoding` are added.
- **Buffered bodies**: the existing path runs unchanged (collect, check `min_size`, try compression, return whichever is smaller).

## Body type

Both `StreamResponse` and `SseResponse` produce a `Response<BoxBody>`, the same type every other handler returns. Middleware that doesn't touch the body (auth, CORS, trace IDs, request logging) passes them through untouched. Middleware that DOES need to read the body checks `body.size_hint().exact()` and skips streaming bodies if buffering would be wrong (cache does this; compression handles streaming explicitly).

If you're implementing your own middleware and need to check whether a response is buffered or streaming:

```rust
use hyper::body::Body as _;

if response.body().size_hint().exact().is_none() {
    // streaming, pass through, don't try to buffer
    return response;
}
```

## Troubleshooting

**Events arrive only after the handler finishes.** Check that you're returning `SseResponse::new(stream)` and not `Vec<SseEvent>::into_response()`. The latter buffers. If you're behind nginx, confirm `proxy_buffering off` for the route. The `X-Accel-Buffering: no` header takes care of this for nginx versions that respect it, but some proxy configs still buffer.

**Browser EventSource keeps reconnecting.** Most likely the connection is being closed by an upstream proxy after an idle timeout. Set `SseResponse::new(...).keep_alive(Some(Duration::from_secs(N)))` to a value below the proxy timeout (15s default works for almost all proxies).

**Compression broke my stream.** Two cases:
- *SSE*: should never happen. `text/event-stream` is unconditionally skipped. If you see `Content-Encoding: gzip` on an SSE response, file a bug.
- *Other streaming bodies*: the wire bytes are valid gzip, but some HTTP clients don't decompress chunked gzip incrementally. `curl` with `--compressed` or any standard browser handles it fine. `gunzip` on a saved chunked-decoded stream also works. If you're hitting "unexpected EOF" on a custom client, make sure it's reading until the connection closes (or the chunked terminator) before passing bytes to the gunzip layer.

**Server panics with "encoder present until upstream end".** Indicates the compression body's invariant was violated. Should be unreachable; report a bug with the producer code.

**Client disconnect leaks tasks.** It shouldn't. `SseBody` and `StreamingCompressedBody` only own the producer stream and a timer, both dropped when the body is dropped. If you see leaks, check that your producer stream itself doesn't hold a `tokio::spawn` handle that outlives the response. Use `async_stream::stream!` rather than spawning a separate task.

**Mid-handler error.** Yield `Err(BodyError)` from the producer stream:

```rust
let s = async_stream::stream! {
    yield Ok::<_, BodyError>(Bytes::from_static(b"start"));
    yield Err(Box::new(std::io::Error::other("boom")) as BodyError);
};
```

The connection will be closed without a clean termination chunk. The client sees a truncated response. There's no way to send a structured error mid-stream once the headers are committed. That's a property of HTTP/1.1, not Rapina.

## Body error type

`BodyError` is `Box<dyn std::error::Error + Send + Sync>`. Part of the public middleware contract: any code that calls `body.collect().await` or polls frames from a Rapina response body receives this type. Producers convert their own errors via `Box::new(err)` or `err.into()`.
