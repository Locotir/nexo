//! Vault: a tiny multi-user document store with a request-handler layer.
//!
//! The layers, bottom to top:
//!
//! - `users`, `auth`: who the callers are and how they prove it.
//! - `store`, `quota`, `audit`: the documents, what each user may keep, and what happened.
//! - `query`: a small filter language evaluated over the documents.
//! - `files`, `export`: attachments on disk, and documents rendered to text, Markdown or JSON.
//! - `service`: the rules (who may read, change or delete what) on top of the storage.
//! - `wire`, `handler`: the binary frames and the text commands a front end sends.
//!
//! The digests used for passwords and tokens are small stand-ins for real ones, so the crate
//! needs no dependencies; their strength is not what this crate is about.

pub mod audit;
pub mod auth;
pub mod error;
pub mod export;
pub mod files;
pub mod handler;
pub mod query;
pub mod quota;
pub mod service;
pub mod store;
pub mod text;
pub mod users;
pub mod wire;

pub use error::{Result, VaultError};
