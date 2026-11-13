//! The long tasks: one inventory crate with three bugs, a noisy audit and, for the marathon, five
//! specifications with tables of examples. The history grows well past any curated view, and the
//! last request needs values and facts from the very first one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use super::{always, manifest, Task};
use crate::check::{read, text, unchanged, Copy, Outcome, Part};
use crate::rng::Rng;

/// The long-haul and marathon tasks, and the marathon with twenty times the examples.
pub fn tasks() -> Vec<Task> {
    vec![long_haul(), marathon(), marathon_heavy(), marathon_noisy(), marathon_pasted()]
}

mod crate_sources;
use crate_sources::*;
mod crate_tests;
use crate_tests::*;
mod long_haul;
use long_haul::*;
mod marathon_spec;
use marathon_spec::*;
mod marathon_tasks;
use marathon_tasks::*;
mod noisy_and_pasted;
use noisy_and_pasted::*;
mod marathon_check;
use marathon_check::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod heavy;
