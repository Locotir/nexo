//! A stream that ends at its first `Done`.

use nexo_core::{EventStream, StreamEvent};

/// Pins a stream and stops it after the first `Done`, which every adapter's
/// stream ends with.
pub(crate) fn terminated(s: impl futures::Stream<Item = StreamEvent> + Send + 'static) -> EventStream {
    Box::pin(TerminatedStream { inner: Box::pin(s), done: false })
}

struct TerminatedStream {
    inner: EventStream,
    done: bool,
}

impl futures::Stream for TerminatedStream {
    type Item = StreamEvent;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<StreamEvent>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        match self.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(StreamEvent::Done)) => {
                self.done = true;
                std::task::Poll::Ready(Some(StreamEvent::Done))
            }
            other => other,
        }
    }
}
