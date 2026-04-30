//! Streaming response bodies: raw byte streams and Server-Sent Events.
//!
//! Both types implement [`IntoResponse`] and produce a [`BoxBody`] that
//! `compression` and `cache` middleware will skip via either the SSE
//! content-type rule or the `size_hint().exact() == None` rule.

use std::fmt::Write as _;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, header};
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Body, Frame, SizeHint};
use pin_project_lite::pin_project;

use super::{BodyError, BoxBody, IntoResponse};

type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;
type SseStream = BoxStream<Result<SseEvent, BodyError>>;
type ByteStream = BoxStream<Result<Bytes, BodyError>>;

/// Streaming response of arbitrary chunked bytes.
///
/// Wraps any [`Stream`] of `Result<Bytes, BodyError>` so frames are written
/// to the wire as they arrive. Content-Type defaults to
/// `application/octet-stream`; override with [`StreamResponse::content_type`].
///
/// ```ignore
/// use rapina::response::{BodyError, StreamResponse};
/// use bytes::Bytes;
///
/// async fn handler() -> StreamResponse {
///     let stream = async_stream::stream! {
///         yield Ok::<_, BodyError>(Bytes::from_static(b"hello "));
///         yield Ok(Bytes::from_static(b"world"));
///     };
///     StreamResponse::new(stream)
/// }
/// ```
pub struct StreamResponse {
    stream: ByteStream,
    status: StatusCode,
    content_type: HeaderValue,
    extra_headers: HeaderMap,
}

impl StreamResponse {
    /// Wrap a stream of `Result<Bytes, BodyError>` into a streaming response.
    pub fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, BodyError>> + Send + 'static,
    {
        Self {
            stream: Box::pin(stream),
            status: StatusCode::OK,
            content_type: HeaderValue::from_static("application/octet-stream"),
            extra_headers: HeaderMap::new(),
        }
    }

    /// Build a streaming response from a `tokio::sync::mpsc::Receiver`. The
    /// receiver carries `Result<Bytes, BodyError>` so producers can signal
    /// errors mid-stream. When the sender side drops, the stream ends.
    ///
    /// ```ignore
    /// let (tx, rx) = tokio::sync::mpsc::channel(32);
    /// tokio::spawn(async move {
    ///     tx.send(Ok(Bytes::from_static(b"chunk"))).await.unwrap();
    /// });
    /// StreamResponse::from_receiver(rx)
    /// ```
    pub fn from_receiver(rx: tokio::sync::mpsc::Receiver<Result<Bytes, BodyError>>) -> Self {
        Self::new(ReceiverStream::new(rx))
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Set the response `Content-Type`.
    ///
    /// Accepts anything convertible to a [`HeaderValue`], so callers can pass
    /// a `&str`, a `String`, or a pre-built `HeaderValue`. Conversion failure
    /// at construction time panics; if you need fallible construction, build
    /// the `HeaderValue` yourself and pass it in.
    pub fn content_type<V>(mut self, content_type: V) -> Self
    where
        V: TryInto<HeaderValue>,
        V::Error: std::fmt::Debug,
    {
        self.content_type = content_type
            .try_into()
            .expect("StreamResponse::content_type: invalid header value");
        self
    }

    /// Add an arbitrary response header. Useful for `Content-Disposition`,
    /// `ETag`, custom headers, etc. Mirrors the API of `Response::builder()`.
    pub fn header<K, V>(mut self, name: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: std::fmt::Debug,
        V: TryInto<HeaderValue>,
        V::Error: std::fmt::Debug,
    {
        let name = name
            .try_into()
            .expect("StreamResponse::header: invalid header name");
        let value = value
            .try_into()
            .expect("StreamResponse::header: invalid header value");
        self.extra_headers.append(name, value);
        self
    }
}

impl IntoResponse for StreamResponse {
    fn into_response(self) -> Response<BoxBody> {
        let frames = FrameStream { inner: self.stream };
        let body = StreamBody::new(frames).boxed_unsync();
        let mut response = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type)
            .body(body)
            .expect("status and content-type are pre-validated");
        response.headers_mut().extend(self.extra_headers);
        response
    }
}

pin_project! {
    /// Adapter: lifts `Stream<Item = Result<Bytes, E>>` to
    /// `Stream<Item = Result<Frame<Bytes>, E>>` without pulling `futures-util`.
    struct FrameStream<S> {
        #[pin]
        inner: S,
    }
}

impl<S> Stream for FrameStream<S>
where
    S: Stream<Item = Result<Bytes, BodyError>>,
{
    type Item = Result<Frame<Bytes>, BodyError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match this.inner.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Ok(b))) => Poll::Ready(Some(Ok(Frame::data(b)))),
            Poll::Ready(Some(Err(e))) => {
                tracing::error!("stream response producer yielded error: {}", e);
                Poll::Ready(Some(Err(e)))
            }
        }
    }
}

/// One Server-Sent Event.
///
/// Build with [`SseEvent::data`] for a normal event, [`SseEvent::comment`]
/// for a comment-only event (browsers ignore comments, useful for keep-alive
/// or operator annotations), or chain [`SseEvent::with_comment`] on top of a
/// data event.
///
/// Multi-line `data` produces multiple `data:` lines per the EventSource spec.
#[derive(Clone, Debug, Default)]
pub struct SseEvent {
    data: Option<String>,
    comment: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: Option<u32>,
}

impl SseEvent {
    /// Event with a data payload. The most common form.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            data: Some(data.into()),
            ..Default::default()
        }
    }

    /// Comment-only event (`: <text>\n\n`). Browsers ignore comments per the
    /// EventSource spec; useful for human-readable annotations in the stream
    /// and for proxies that need traffic to keep the connection warm.
    pub fn comment(text: impl Into<String>) -> Self {
        Self {
            comment: Some(text.into()),
            ..Default::default()
        }
    }

    /// Attach a comment to a data event. Renders as a `:` line before the
    /// `data:` lines in the encoded output.
    pub fn with_comment(mut self, text: impl Into<String>) -> Self {
        self.comment = Some(text.into());
        self
    }

    pub fn event(mut self, name: impl Into<String>) -> Self {
        self.event = Some(name.into());
        self
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn retry(mut self, ms: u32) -> Self {
        self.retry = Some(ms);
        self
    }

    /// Encode this event in the `text/event-stream` wire format.
    ///
    /// Field order: `comment`, `event`, `id`, `retry`, `data` (one line per
    /// line of input), terminated by a blank line. LF only, no CR.
    pub fn encode(&self) -> Bytes {
        let mut out = String::with_capacity(self.data.as_deref().map_or(0, str::len) + 32);
        if let Some(c) = &self.comment {
            for line in c.split('\n') {
                out.push_str(": ");
                out.push_str(line);
                out.push('\n');
            }
        }
        if let Some(name) = &self.event {
            out.push_str("event: ");
            out.push_str(name);
            out.push('\n');
        }
        if let Some(id) = &self.id {
            out.push_str("id: ");
            out.push_str(id);
            out.push('\n');
        }
        if let Some(retry) = self.retry {
            let _ = writeln!(out, "retry: {retry}");
        }
        if let Some(data) = &self.data {
            for line in data.split('\n') {
                out.push_str("data: ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push('\n');
        Bytes::from(out)
    }
}

const SSE_KEEP_ALIVE_DEFAULT: Duration = Duration::from_secs(15);
const SSE_KEEP_ALIVE_FRAME_DEFAULT: &[u8] = b":\n\n";

/// Server-Sent Events response.
///
/// Wraps a stream of `Result<SseEvent, BodyError>` and interleaves
/// keep-alive comment frames when the user stream is idle for longer than
/// the configured interval. Defaults: 15 second interval, `:\n\n` frame.
/// Pass `None` to [`SseResponse::keep_alive`] to disable, or
/// [`SseResponse::keep_alive_text`] to customize the frame text.
///
/// Sets `Content-Type: text/event-stream`, `Cache-Control: no-cache`, and
/// `X-Accel-Buffering: no` so reverse proxies do not buffer the stream.
pub struct SseResponse {
    stream: SseStream,
    status: StatusCode,
    keep_alive: Option<Duration>,
    keep_alive_frame: Bytes,
    extra_headers: HeaderMap,
}

impl SseResponse {
    /// Wrap a stream of `Result<SseEvent, BodyError>`.
    pub fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<SseEvent, BodyError>> + Send + 'static,
    {
        Self {
            stream: Box::pin(stream),
            status: StatusCode::OK,
            keep_alive: Some(SSE_KEEP_ALIVE_DEFAULT),
            keep_alive_frame: Bytes::from_static(SSE_KEEP_ALIVE_FRAME_DEFAULT),
            extra_headers: HeaderMap::new(),
        }
    }

    /// Build an SSE response from any iterator of events. Convenient when
    /// you already have a finite list of events to send and don't want to
    /// reach for `async_stream::stream!`.
    ///
    /// ```ignore
    /// let events = vec![SseEvent::data("a"), SseEvent::data("b")];
    /// SseResponse::from_events(events)
    /// ```
    pub fn from_events<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = SseEvent>,
        I::IntoIter: Send + 'static,
    {
        Self::new(IterStream::new(iter.into_iter()))
    }

    /// Build an SSE response from a `tokio::sync::mpsc::Receiver`. The
    /// receiver carries `Result<SseEvent, BodyError>` so producers can
    /// signal errors mid-stream. When the sender drops, the stream ends.
    ///
    /// ```ignore
    /// let (tx, rx) = tokio::sync::mpsc::channel(32);
    /// tokio::spawn(async move {
    ///     tx.send(Ok(SseEvent::data("hi"))).await.unwrap();
    /// });
    /// SseResponse::from_receiver(rx)
    /// ```
    pub fn from_receiver(rx: tokio::sync::mpsc::Receiver<Result<SseEvent, BodyError>>) -> Self {
        Self::new(ReceiverStream::new(rx))
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Set or disable the keep-alive interval. Default is 15 seconds.
    pub fn keep_alive(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive = interval;
        self
    }

    /// Customize the keep-alive payload. Default is `:\n\n` (an empty
    /// EventSource-spec comment, which browsers ignore). The payload is
    /// always wrapped in the comment prefix and trailing blank line, so
    /// pass just the comment text. For example, `keep_alive_text("ping")`
    /// emits `: ping\n\n`.
    pub fn keep_alive_text(mut self, text: impl AsRef<str>) -> Self {
        let s = text.as_ref();
        let mut out = String::with_capacity(s.len() + 4);
        out.push_str(": ");
        out.push_str(s);
        out.push_str("\n\n");
        self.keep_alive_frame = Bytes::from(out);
        self
    }

    /// Add an arbitrary response header.
    pub fn header<K, V>(mut self, name: K, value: V) -> Self
    where
        K: TryInto<HeaderName>,
        K::Error: std::fmt::Debug,
        V: TryInto<HeaderValue>,
        V::Error: std::fmt::Debug,
    {
        let name = name
            .try_into()
            .expect("SseResponse::header: invalid header name");
        let value = value
            .try_into()
            .expect("SseResponse::header: invalid header value");
        self.extra_headers.append(name, value);
        self
    }
}

impl IntoResponse for SseResponse {
    fn into_response(self) -> Response<BoxBody> {
        let body = SseBody {
            stream: self.stream,
            keep_alive: self.keep_alive.map(KeepAliveState::new),
            keep_alive_frame: self.keep_alive_frame,
        }
        .boxed_unsync();
        let mut response = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header("X-Accel-Buffering", "no")
            .body(body)
            .expect("status and content-type are pre-validated");
        response.headers_mut().extend(self.extra_headers);
        response
    }
}

struct KeepAliveState {
    interval: Duration,
    sleep: Pin<Box<tokio::time::Sleep>>,
}

impl KeepAliveState {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            sleep: Box::pin(tokio::time::sleep(interval)),
        }
    }

    fn reset(&mut self) {
        self.sleep
            .as_mut()
            .reset(tokio::time::Instant::now() + self.interval);
    }
}

pin_project! {
    /// Custom [`Body`] impl that owns the user stream and the keep-alive timer.
    ///
    /// Polling order on each frame: try the user stream first; on `Pending`,
    /// poll the keep-alive timer; if the timer fires, emit the keep-alive
    /// payload and reset. Logs a debug line when dropped so operators can
    /// spot client disconnects.
    struct SseBody<S> {
        #[pin]
        stream: S,
        keep_alive: Option<KeepAliveState>,
        keep_alive_frame: Bytes,
    }

    impl<S> PinnedDrop for SseBody<S> {
        fn drop(this: Pin<&mut Self>) {
            tracing::debug!("SSE body dropped (client disconnect or stream completion)");
            let _ = this;
        }
    }
}

impl<S> Body for SseBody<S>
where
    S: Stream<Item = Result<SseEvent, BodyError>> + Send + 'static,
{
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let this = self.project();

        match this.stream.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                if let Some(ka) = this.keep_alive.as_mut() {
                    ka.reset();
                }
                Poll::Ready(Some(Ok(Frame::data(event.encode()))))
            }
            Poll::Ready(Some(Err(e))) => {
                tracing::error!("SSE producer yielded error: {}", e);
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => match this.keep_alive.as_mut() {
                Some(ka) => match ka.sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        ka.reset();
                        Poll::Ready(Some(Ok(Frame::data(this.keep_alive_frame.clone()))))
                    }
                    Poll::Pending => Poll::Pending,
                },
                None => Poll::Pending,
            },
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

// Internal stream adapters. Hand-rolled so we don't need tokio-stream as a
// runtime dep just for these two cases.

pin_project! {
    /// Wraps a `tokio::sync::mpsc::Receiver<T>` as a `Stream<Item = T>`.
    struct ReceiverStream<T> {
        rx: tokio::sync::mpsc::Receiver<T>,
    }
}

impl<T> ReceiverStream<T> {
    fn new(rx: tokio::sync::mpsc::Receiver<T>) -> Self {
        Self { rx }
    }
}

impl<T> Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.project().rx.poll_recv(cx)
    }
}

pin_project! {
    /// Wraps an iterator as a `Stream<Item = Result<Item, BodyError>>`. Used
    /// for `SseResponse::from_events`.
    struct IterStream<I, T>
    where
        I: Iterator<Item = T>,
    {
        iter: I,
    }
}

impl<I> IterStream<I, I::Item>
where
    I: Iterator,
{
    fn new(iter: I) -> Self {
        Self { iter }
    }
}

impl<I, T> Stream for IterStream<I, T>
where
    I: Iterator<Item = T>,
{
    type Item = Result<T, BodyError>;

    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        Poll::Ready(this.iter.next().map(Ok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::poll_fn;
    use std::marker::PhantomData;

    /// Stream that yields nothing and completes immediately.
    struct EmptyStream<T>(PhantomData<T>);
    impl<T> Stream for EmptyStream<T> {
        type Item = T;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<T>> {
            Poll::Ready(None)
        }
    }
    fn empty<T>() -> EmptyStream<T> {
        EmptyStream(PhantomData)
    }

    /// Stream that is permanently pending.
    struct PendingStream<T>(PhantomData<T>);
    impl<T> Stream for PendingStream<T> {
        type Item = T;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<T>> {
            Poll::Pending
        }
    }
    fn pending<T>() -> PendingStream<T> {
        PendingStream(PhantomData)
    }

    #[test]
    fn test_sse_event_data_only() {
        let bytes = SseEvent::data("hello").encode();
        assert_eq!(&bytes[..], b"data: hello\n\n");
    }

    #[test]
    fn test_sse_event_multiline_data() {
        let bytes = SseEvent::data("line1\nline2").encode();
        assert_eq!(&bytes[..], b"data: line1\ndata: line2\n\n");
    }

    #[test]
    fn test_sse_event_with_event_name() {
        let bytes = SseEvent::data("payload").event("update").encode();
        assert_eq!(&bytes[..], b"event: update\ndata: payload\n\n");
    }

    #[test]
    fn test_sse_event_with_id() {
        let bytes = SseEvent::data("payload").id("42").encode();
        assert_eq!(&bytes[..], b"id: 42\ndata: payload\n\n");
    }

    #[test]
    fn test_sse_event_with_retry() {
        let bytes = SseEvent::data("payload").retry(5000).encode();
        assert_eq!(&bytes[..], b"retry: 5000\ndata: payload\n\n");
    }

    #[test]
    fn test_sse_event_full() {
        let bytes = SseEvent::data("payload")
            .event("update")
            .id("7")
            .retry(1000)
            .encode();
        assert_eq!(
            &bytes[..],
            b"event: update\nid: 7\nretry: 1000\ndata: payload\n\n"
        );
    }

    #[test]
    fn test_sse_event_comment_only() {
        let bytes = SseEvent::comment("debug-marker").encode();
        assert_eq!(&bytes[..], b": debug-marker\n\n");
    }

    #[test]
    fn test_sse_event_data_with_comment() {
        let bytes = SseEvent::data("payload").with_comment("annotated").encode();
        assert_eq!(&bytes[..], b": annotated\ndata: payload\n\n");
    }

    #[test]
    fn test_sse_event_multiline_comment() {
        let bytes = SseEvent::comment("line1\nline2").encode();
        assert_eq!(&bytes[..], b": line1\n: line2\n\n");
    }

    #[tokio::test]
    async fn test_stream_response_size_hint_is_none() {
        let s = empty::<Result<Bytes, BodyError>>();
        let resp = StreamResponse::new(s).into_response();
        assert_eq!(resp.body().size_hint().exact(), None);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn test_stream_response_content_type_setters() {
        let resp = StreamResponse::new(empty::<Result<Bytes, BodyError>>())
            .content_type("application/pdf")
            .into_response();
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/pdf"
        );

        let dynamic = HeaderValue::from_str("application/x-custom").unwrap();
        let resp = StreamResponse::new(empty::<Result<Bytes, BodyError>>())
            .content_type(dynamic)
            .into_response();
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-custom"
        );
    }

    #[tokio::test]
    async fn test_stream_response_extra_headers() {
        let resp = StreamResponse::new(empty::<Result<Bytes, BodyError>>())
            .header("content-disposition", "attachment; filename=\"x.pdf\"")
            .header("etag", "\"abc\"")
            .into_response();
        assert_eq!(
            resp.headers().get("content-disposition").unwrap(),
            "attachment; filename=\"x.pdf\""
        );
        assert_eq!(resp.headers().get("etag").unwrap(), "\"abc\"");
    }

    #[tokio::test]
    async fn test_sse_response_headers_and_size_hint() {
        let resp = SseResponse::new(empty::<Result<SseEvent, BodyError>>()).into_response();
        assert_eq!(resp.body().size_hint().exact(), None);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache"
        );
        assert_eq!(resp.headers().get("X-Accel-Buffering").unwrap(), "no");
    }

    #[tokio::test]
    async fn test_sse_response_extra_headers() {
        let resp = SseResponse::new(empty::<Result<SseEvent, BodyError>>())
            .header("x-trace-id", "abc123")
            .into_response();
        assert_eq!(resp.headers().get("x-trace-id").unwrap(), "abc123");
    }

    /// Regression: HeaderMap iteration yields `(None, value)` for repeated header
    /// names, so naive `filter_map` on `Option<HeaderName>` drops every value
    /// after the first. Both response types must round-trip multi-value headers.
    #[tokio::test]
    async fn test_stream_response_preserves_multi_value_headers() {
        let resp = StreamResponse::new(empty::<Result<Bytes, BodyError>>())
            .header(header::SET_COOKIE, "session=abc; Path=/")
            .header(header::SET_COOKIE, "csrf=xyz; Path=/")
            .into_response();
        let cookies: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(cookies.len(), 2, "both Set-Cookie values must survive");
        assert!(
            cookies
                .iter()
                .any(|v| v.to_str().unwrap().contains("session=abc"))
        );
        assert!(
            cookies
                .iter()
                .any(|v| v.to_str().unwrap().contains("csrf=xyz"))
        );
    }

    #[tokio::test]
    async fn test_sse_response_preserves_multi_value_headers() {
        let resp = SseResponse::new(empty::<Result<SseEvent, BodyError>>())
            .header(header::SET_COOKIE, "session=abc; Path=/")
            .header(header::SET_COOKIE, "csrf=xyz; Path=/")
            .into_response();
        let cookies: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(cookies.len(), 2, "both Set-Cookie values must survive");
    }

    #[tokio::test]
    async fn test_sse_response_status_setter() {
        let resp = SseResponse::new(empty::<Result<SseEvent, BodyError>>())
            .status(StatusCode::ACCEPTED)
            .into_response();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    /// Drives `SseBody` directly: idle producer + short keep_alive must yield
    /// a `:\n\n` frame within roughly the keep-alive interval.
    #[tokio::test(flavor = "current_thread")]
    async fn test_sse_keep_alive_emits_default_comment_frame_when_idle() {
        let body = SseBody {
            stream: pending::<Result<SseEvent, BodyError>>(),
            keep_alive: Some(KeepAliveState::new(Duration::from_millis(50))),
            keep_alive_frame: Bytes::from_static(SSE_KEEP_ALIVE_FRAME_DEFAULT),
        };
        let mut body = std::pin::pin!(body);
        let frame: Option<Result<Frame<Bytes>, BodyError>> =
            poll_fn(|cx| body.as_mut().poll_frame(cx)).await;
        let frame = frame
            .expect("expected a frame")
            .expect("frame yielded error");
        let data = frame.into_data().expect("expected data frame");
        assert_eq!(&data[..], b":\n\n");
    }

    /// Custom keep-alive payload renders as `: <text>\n\n`.
    #[tokio::test(flavor = "current_thread")]
    async fn test_sse_keep_alive_text_emits_custom_comment_frame() {
        let body = SseBody {
            stream: pending::<Result<SseEvent, BodyError>>(),
            keep_alive: Some(KeepAliveState::new(Duration::from_millis(50))),
            keep_alive_frame: {
                // Construct the same way the public setter does.
                let s = "ping";
                let mut out = String::with_capacity(s.len() + 4);
                out.push_str(": ");
                out.push_str(s);
                out.push_str("\n\n");
                Bytes::from(out)
            },
        };
        let mut body = std::pin::pin!(body);
        let frame: Option<Result<Frame<Bytes>, BodyError>> =
            poll_fn(|cx| body.as_mut().poll_frame(cx)).await;
        let frame = frame
            .expect("expected a frame")
            .expect("frame yielded error");
        let data = frame.into_data().expect("expected data frame");
        assert_eq!(&data[..], b": ping\n\n");
    }

    /// Verify the public setter produces the right keep-alive frame.
    #[test]
    fn test_keep_alive_text_setter_builds_correct_frame() {
        let resp = SseResponse::new(empty::<Result<SseEvent, BodyError>>()).keep_alive_text("ping");
        assert_eq!(&resp.keep_alive_frame[..], b": ping\n\n");
    }

    /// Producer error path: yield Err and observe the body returning Some(Err).
    #[tokio::test(flavor = "current_thread")]
    async fn test_sse_propagates_producer_error() {
        let s = async_stream::stream! {
            yield Err(Box::new(std::io::Error::other("boom")) as BodyError);
        };
        let body = SseBody {
            stream: s,
            keep_alive: None,
            keep_alive_frame: Bytes::from_static(SSE_KEEP_ALIVE_FRAME_DEFAULT),
        };
        let mut body = std::pin::pin!(body);
        let frame: Option<Result<Frame<Bytes>, BodyError>> =
            poll_fn(|cx| body.as_mut().poll_frame(cx)).await;
        match frame {
            Some(Err(e)) => assert!(e.to_string().contains("boom")),
            Some(Ok(_)) => panic!("expected error, got data frame"),
            None => panic!("expected error, got end of stream"),
        }
    }

    /// Round-trip a finite list of events through `SseResponse::from_events`.
    #[tokio::test(flavor = "current_thread")]
    async fn test_sse_from_events_emits_events_in_order() {
        let resp =
            SseResponse::from_events(vec![SseEvent::data("first"), SseEvent::data("second")])
                .keep_alive(None)
                .into_response();
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let s = std::str::from_utf8(&body_bytes).unwrap();
        assert!(s.contains("data: first"));
        assert!(s.contains("data: second"));
        assert!(s.find("first").unwrap() < s.find("second").unwrap());
    }

    /// Round-trip a tokio mpsc channel through `SseResponse::from_receiver`.
    #[tokio::test(flavor = "current_thread")]
    async fn test_sse_from_receiver() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            tx.send(Ok(SseEvent::data("via channel"))).await.unwrap();
            // dropping tx ends the stream
        });
        let resp = SseResponse::from_receiver(rx)
            .keep_alive(None)
            .into_response();
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(body_bytes.windows(11).any(|w| w == b"via channel"));
    }

    /// Round-trip a tokio mpsc channel through `StreamResponse::from_receiver`.
    #[tokio::test(flavor = "current_thread")]
    async fn test_stream_from_receiver() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            tx.send(Ok(Bytes::from_static(b"via channel")))
                .await
                .unwrap();
        });
        let resp = StreamResponse::from_receiver(rx)
            .content_type("text/plain")
            .into_response();
        let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body_bytes[..], b"via channel");
    }
}
