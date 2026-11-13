//! Shared by `examples/scale.rs` and `tests/scale.rs`: synthetic sessions of four shapes, fed to the curator
//! one request at a time as the host does, and the numbers measured on the way.
#![allow(dead_code, unused_imports)]

use std::collections::VecDeque;
use std::time::Instant;

use nexo_curator::Curator;
use serde_json::{json, Value};

/// The spec the runs measure: the default one, or the settings `NEXO_SCALE_SPEC` gives over it.
pub(super) fn spec() -> String {
    std::env::var("NEXO_SCALE_SPEC").unwrap_or_default()
}

mod cold_params;
mod cold_row;
mod cold_session;
mod saw;
mod session_gen;
mod timing_run;
pub use cold_params::*;
pub use cold_row::*;
pub use cold_session::*;
pub use saw::*;
pub use session_gen::*;
pub use timing_run::*;
