//! Server-sent events and newline-delimited JSON, as the providers stream
//! them, parsed from chunks that may be cut anywhere.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::Stream;

/// One event of the stream: the `event:` name (if any) and the joined `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Bytes of one line kept before it is cut (no provider sends lines this long).
pub const MAX_LINE: usize = 8 * 1024 * 1024;

/// An incremental SSE parser: feed it bytes, take the events that completed.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    id: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// Takes a chunk and returns the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if self.buf.len() > MAX_LINE && !self.buf.contains(&b'\n') {
            // a body with no line ends at all (a page, a binary): bounded memory, one line
            self.buf.push(b'\n');
        }
        while let Some((line, rest)) = next_line(&self.buf) {
            let line = String::from_utf8_lossy(line).into_owned();
            self.buf.drain(..rest);
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        out
    }

    /// Dispatches what is pending at the end of the stream.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            if let Some(ev) = self.line(&line) {
                return Some(ev);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => self.data.push(value.to_string()),
            "event" => self.event = Some(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            "retry" => {}
            _ => {
                // a 200 whose body is plain JSON (an error some gateways send without
                // SSE framing, or a whole response): the line is the data
                let t = line.trim_start();
                if t.starts_with('{') || t.starts_with('[') {
                    self.data.push(line.to_string());
                    return self.dispatch();
                }
            }
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() {
            self.event = None;
            return None;
        }
        let ev = SseEvent { event: self.event.take(), data: self.data.join("\n"), id: self.id.clone() };
        self.data.clear();
        Some(ev)
    }
}

/// The first complete line of `buf` (without its terminator) and where the
/// next line starts; `\n`, `\r\n` and lone `\r` all end a line. A lone `\r`
/// at the very end waits for the next chunk, which may bring its `\n`.
fn next_line(buf: &[u8]) -> Option<(&[u8], usize)> {
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            return Some((&buf[..i], i + 1));
        }
        if b == b'\r' {
            if i + 1 < buf.len() {
                let skip = if buf[i + 1] == b'\n' { 2 } else { 1 };
                return Some((&buf[..i], i + skip));
            }
            return None;
        }
    }
    None
}

/// Splits a byte stream into lines (for newline-delimited JSON).
#[derive(Debug, Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((line, rest)) = next_line(&self.buf) {
            let s = String::from_utf8_lossy(line).trim().to_string();
            self.buf.drain(..rest);
            if !s.is_empty() {
                out.push(s);
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<String> {
        let s = String::from_utf8_lossy(&self.buf).trim().to_string();
        self.buf.clear();
        (!s.is_empty()).then_some(s)
    }
}

/// What a byte stream yields: bytes, or the transport's error text.
pub type ByteResult = Result<Bytes, String>;

/// Wraps a byte stream as a stream of SSE events (an `Err` is the transport's
/// error, passed through once; the stream ends after it).
pub struct SseStream<S> {
    inner: Option<Pin<Box<S>>>,
    parser: SseParser,
    pending: VecDeque<Result<SseEvent, String>>,
}

impl<S: Stream<Item = ByteResult>> SseStream<S> {
    pub fn new(inner: S) -> SseStream<S> {
        SseStream { inner: Some(Box::pin(inner)), parser: SseParser::new(), pending: VecDeque::new() }
    }
}

impl<S: Stream<Item = ByteResult>> Stream for SseStream<S> {
    type Item = Result<SseEvent, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Poll::Ready(Some(ev));
            }
            let Some(inner) = self.inner.as_mut() else { return Poll::Ready(None) };
            match inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    let evs = self.parser.push(&chunk);
                    self.pending.extend(evs.into_iter().map(Ok));
                }
                Poll::Ready(Some(Err(e))) => {
                    self.inner = None;
                    if let Some(ev) = self.parser.finish() {
                        self.pending.push_back(Ok(ev));
                    }
                    self.pending.push_back(Err(e));
                }
                Poll::Ready(None) => {
                    self.inner = None;
                    if let Some(ev) = self.parser.finish() {
                        self.pending.push_back(Ok(ev));
                    }
                }
            }
        }
    }
}

/// Wraps a byte stream as a stream of NDJSON lines.
pub struct LineStream<S> {
    inner: Option<Pin<Box<S>>>,
    splitter: LineSplitter,
    pending: VecDeque<Result<String, String>>,
}

impl<S: Stream<Item = ByteResult>> LineStream<S> {
    pub fn new(inner: S) -> LineStream<S> {
        LineStream { inner: Some(Box::pin(inner)), splitter: LineSplitter::default(), pending: VecDeque::new() }
    }
}

impl<S: Stream<Item = ByteResult>> Stream for LineStream<S> {
    type Item = Result<String, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(l) = self.pending.pop_front() {
                return Poll::Ready(Some(l));
            }
            let Some(inner) = self.inner.as_mut() else { return Poll::Ready(None) };
            match inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(chunk))) => {
                    let lines = self.splitter.push(&chunk);
                    self.pending.extend(lines.into_iter().map(Ok));
                }
                Poll::Ready(Some(Err(e))) => {
                    self.inner = None;
                    if let Some(l) = self.splitter.finish() {
                        self.pending.push_back(Ok(l));
                    }
                    self.pending.push_back(Err(e));
                }
                Poll::Ready(None) => {
                    self.inner = None;
                    if let Some(l) = self.splitter.finish() {
                        self.pending.push_back(Ok(l));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_split_anywhere() {
        let text = "event: message_start\r\ndata: {\"a\":1}\r\n\r\n: comment\ndata: line one\ndata: line two\n\ndata: [DONE]\n\n";
        let mut p = SseParser::new();
        let mut all = Vec::new();
        // feed one byte at a time: the cruelest split
        for b in text.as_bytes() {
            all.extend(p.push(&[*b]));
        }
        all.extend(p.finish());
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].event.as_deref(), Some("message_start"));
        assert_eq!(all[0].data, "{\"a\":1}");
        assert_eq!(all[1].event, None);
        assert_eq!(all[1].data, "line one\nline two");
        assert_eq!(all[2].data, "[DONE]");
    }

    #[test]
    fn a_json_line_without_a_field_is_data() {
        let mut p = SseParser::new();
        let evs = p.push(b"{\"error\":{\"message\":\"no\"}}\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "{\"error\":{\"message\":\"no\"}}");
        let mut p = SseParser::new();
        assert!(p.push(b"<html><body>502</body></html>\n").is_empty(), "markup is not data");
        assert!(p.finish().is_none());
    }

    #[test]
    fn an_endless_line_is_cut_at_the_limit() {
        let mut p = SseParser::new();
        let chunk = vec![b'x'; MAX_LINE + 10];
        assert!(p.push(&chunk).is_empty());
        assert!(p.buf.len() <= 16, "the buffer does not grow without bound: {}", p.buf.len());
    }

    #[test]
    fn dispatches_a_trailing_event_without_blank_line() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: tail").is_empty());
        assert_eq!(p.finish().unwrap().data, "tail");
    }

    #[test]
    fn splits_ndjson_lines() {
        let mut s = LineSplitter::default();
        let mut out = s.push(b"{\"a\":1}\n{\"b\"");
        out.extend(s.push(b":2}\r\n\n{\"c\":3}"));
        out.extend(s.finish());
        assert_eq!(out, vec!["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"]);
    }

    #[tokio::test]
    async fn sse_stream_passes_a_transport_error_through_once() {
        use futures::StreamExt;
        let chunks: Vec<ByteResult> = vec![Ok(Bytes::from("data: a\n\ndata: b")), Err("cut".into())];
        let items: Vec<_> = SseStream::new(futures::stream::iter(chunks)).collect().await;
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_ref().unwrap().data, "a");
        assert_eq!(items[1].as_ref().unwrap().data, "b");
        assert_eq!(items[2].as_ref().unwrap_err(), "cut");
    }
}
