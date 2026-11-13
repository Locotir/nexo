//! The native-messaging host of the nexo browser extension. Chrome starts it when the extension connects and talks
//! to it over stdio (4-byte length + JSON). It opens a local endpoint (see `nexo_relay::endpoint`) where nexo agents
//! connect, and passes messages between the two, with three rules it enforces itself whatever the extension does:
//!
//! 1. An agent must prove it knows the install's token before it can say anything.
//! 2. Nothing reaches the browser for a session until the person has allowed it in the browser (`connect`).
//! 3. A session can act only on the tabs it created (or that those tabs opened).
//!
//! The wire format is described in `nexo_relay::proto`.

mod bridge;

pub use bridge::{run, run_stdio};
