//! Fitting the behaviour parameters from our own bench runs, and checking the simulator against held-out
//! pairs (`SIMULATOR-SPEC.md` 3 and 5.2).
//!
//! A run directory holds `results.jsonl` (one row per run: task, arm, rep, commit, spec, steps, cost, score,
//! stop, usage, counters) and one transcript per run. The transcript of a curator arm already holds the
//! curator's marks, so the curator is not run on it again: the induced re-fetches are read off its marks.
//! The events come from the PLAIN twin of the pair (same task, rep, commit), fed through the curator with
//! the curator run's own spec, which is exactly what `predict` does later.
//!
//! * `r`: unique cut results fetched back (a `recall`, or a call identical to an earlier one whose result
//!   was out of view when it was made) minus the in-view repeat rate of the family times the exposure,
//!   over the opportunities: the plain twin's `cited` and `denied` events.
//! * `beta`: `(steps_cur - steps_plain - fetches) / unfetched events`, pooled over the pairs, clipped at 0.
//! * `phi`: share of pairs with an unfetched event whose score fell, above the same share among pairs with
//!   no event, turned from a per-run into a per-event chance.
//! * `pi`: recall calls over fetches, in runs that had the tool.
//!
//! Intervals are percentile bootstraps over runs (2000 resamples); a count of zero gets the rule of three.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::behave::{analytic, auto_family, probe, Context, ParamSet};
use super::events::{run, Kind, Tool};
use super::input::{load, reply_to, Request};
use super::price::{tokens_of, Cache, Prices};

mod analysis;
mod bootstrap;
mod dirs_fit;
mod group_fit;
mod holdout_check;
mod pairing;
mod prediction;
mod price_check;
mod rows;
pub use analysis::*;
pub use bootstrap::*;
pub use dirs_fit::*;
pub use group_fit::*;
pub use holdout_check::*;
pub use pairing::*;
pub use prediction::*;
pub use price_check::*;
pub use rows::*;
