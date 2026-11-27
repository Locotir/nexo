//! The providers of nexo: one adapter per *wire protocol*, vendors as
//! presets, and a client that retries. See `docs/providers-base.md` for the
//! protocols and `nexo/ARCHITECTURE.md` for the API this crate commits to.
//!
//! Adding a vendor that speaks one of the protocols here is a
//! [`nexo_core::ProviderConfig`] (a base URL, a key variable), nothing more.
//! Adding a protocol is one module implementing [`nexo_core::Protocol`] and
//! one line in [`registry`].

pub mod anthropic;
pub mod catalogue;
pub mod client;
pub mod codex;
pub mod copilot;
pub mod directory;
pub mod gemini;
pub mod hints;
mod http;
pub mod keys;
pub mod login;
pub mod mock;
pub mod modellist;
pub mod openai_chat;
pub mod openai_responses;
mod output;
pub mod parse;
pub mod presets;
pub mod probe;
pub mod sse;
pub mod testing;
pub use nexo_http::tls;
pub mod usage;
pub mod vendors;
#[cfg(test)]
mod vendors_pin;
pub mod wire;
mod wire_report;

pub use client::{registry, Client, Registry};
pub use nexo_http::build_client;
pub use presets::presets;

/// Flatten nested tool-result payloads without cloning image data. Nested results are
/// payload, not independent protocol calls, so only their leaves go on the wire.
fn result_parts(parts: &[nexo_core::Part]) -> Vec<&nexo_core::Part> {
    let mut out = Vec::new();
    let mut stack = vec![parts.iter()];
    while let Some(iter) = stack.last_mut() {
        match iter.next() {
            Some(nexo_core::Part::ToolResult { content, .. }) => stack.push(content.iter()),
            Some(part) => out.push(part),
            None => {
                stack.pop();
            }
        }
    }
    out
}

fn result_text(parts: &[&nexo_core::Part]) -> String {
    parts.iter().filter_map(|p| p.as_text()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
fn nested_image_result() -> nexo_core::Message {
    use nexo_core::{ImageSource, Message, Part};
    let image = Part::Image { source: ImageSource::Url { url: "https://example.invalid/p.png".into() } };
    Message::tool_results(vec![Part::ToolResult {
        call_id: "outer".into(),
        name: None,
        is_error: true,
        content: vec![
            Part::text("before"),
            Part::ToolResult {
                call_id: "inner".into(),
                name: None,
                is_error: false,
                content: vec![Part::text("nested"), image],
            },
            Part::text("after"),
        ],
    }])
}
