//! The small HTTP helpers that both the providers and the tools need: the
//! one place a TLS client is built ([`tls`]), PKCE for OAuth browser logins
//! ([`pkce`]) and form-body encoding ([`form`]).
//!
//! It is a separate crate so that a tool can make HTTP calls, and sign in
//! to an MCP server with OAuth, without depending on `nexo-providers` and
//! every protocol adapter inside it. `nexo-providers` re-exports these items
//! at their old paths.

pub mod pkce;
pub mod tls;

mod form;

pub use form::form;
pub use tls::build_client;
