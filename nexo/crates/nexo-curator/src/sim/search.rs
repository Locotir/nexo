//! The knob search of the session simulator (`SIMULATOR-SPEC.md` 4): a knob space read from `knobs.toml`,
//! Latin-hypercube sampling, tiers of fixtures, the objective J, coordinate descent, k-fold held-out J and
//! the Pareto front over cost, expected extra steps and chance of failing.

use super::behave::{analytic, auto_family, Context, ParamSet};
use super::events::run;
use super::input::{load, Request};
use super::price::{tokens_of, Cache, Prices};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// ---- knob space ----

mod command_line;
mod descent_and_pareto;
mod fixtures;
mod knob_space;
pub use command_line::*;
pub use descent_and_pareto::*;
pub use fixtures::*;
pub use knob_space::*;
