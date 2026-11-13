//! The wire between the nexo agent and the browser extension, shared by the three programs that touch it:
//! the agent (`client`), Chrome's native-messaging host (`nexo-browser-host`) and the tests that stand in for the
//! extension. Nothing here knows about Chrome beyond its framing rules.

pub mod chunk;
pub mod client;
pub mod endpoint;
pub mod frame;
pub mod proto;
pub mod token;

pub use endpoint::{BoxStream, Dirs, Found, Listener};
pub use proto::BrowserInfo;
