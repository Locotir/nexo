//! Replaying recorded responses through the adapters, for tests of this
//! crate and of whoever consumes its events.

use bytes::Bytes;
use nexo_core::EventStream;

use crate::sse::SseStream;
use crate::{anthropic::Anthropic, gemini::Gemini, http, openai_chat::OpenAiChat, openai_responses::OpenAiResponses};

/// `text` as a body delivered in chunks of `chunk` bytes (0: all at once).
pub fn body(text: &str, chunk: usize) -> http::BodyStream {
    let bytes = text.as_bytes();
    let chunks: Vec<_> = if chunk == 0 {
        vec![Ok(Bytes::copy_from_slice(bytes))]
    } else {
        bytes.chunks(chunk).map(|c| Ok(Bytes::copy_from_slice(c))).collect()
    };
    http::body_from(chunks)
}

/// The events the adapter for `protocol` makes of a recorded SSE response,
/// delivered in chunks of `chunk` bytes.
pub fn replay(protocol: &str, text: &str, chunk: usize) -> Option<EventStream> {
    let sse = SseStream::new(body(text, chunk));
    Some(match protocol {
        crate::openai_chat::ID => OpenAiChat::parse(sse),
        crate::openai_responses::ID => OpenAiResponses::parse(sse),
        crate::anthropic::ID => Anthropic::parse(sse),
        crate::gemini::ID => Gemini::parse(sse),
        _ => return None,
    })
}
