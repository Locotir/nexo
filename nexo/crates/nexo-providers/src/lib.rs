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

pub use client::{registry, Client, Registry};
pub use nexo_http::build_client;
pub use presets::presets;
