//! The context curator: decides, call by call, which parts of a conversation the model sees.
//!
//! [`Curator`] is the goal engine run on the live path: it takes the whole conversation on every
//! call, keeps what it learnt of the earlier messages and returns the view to send, the tokens of that view and what it did.

#[cfg(feature = "sim")]
pub mod sim;

#[macro_use]
mod work;

mod curator;
mod engine;
mod evidence;
mod facts;
mod flat;
mod goal;
mod hindsight;
#[cfg(test)]
mod hindsight_labels;
mod index;
mod json;
mod learn;
mod ledger;
mod mechanical;
mod memory;
mod msg;
pub mod optimum;
mod params;
mod rules;
#[cfg(test)]
mod rules_battery;
pub mod snapshot;
mod sorted;
mod standing;
mod textops;
mod tools;
mod words;

pub use curator::{Curator, Reply};
pub use engine::{keywords, label, Added, AddedKind, AddedState};
pub use memory::{Cell, Memory, State, Zone};
pub use msg::{
    goal_request, ATTACHED, GOAL_CONTINUE_MARK, GOAL_MET_HEAD, GOAL_PAUSED_HEAD, GOAL_SET_MARK, INCLUDED, PERSON_MARK,
    START_NOTE_HEAD,
};
pub use params::D_SPEC;
pub use words::{distinctive_words, word_kind, LostTally, WordKind};

// The tests reach the curator's private parts through `super::*`.
#[cfg(test)]
use {
    curator::page,
    params::{params, parse_params, FLOATS, INTS, ONLY, STRS},
};

/// Names any model knows without reading them in the conversation, one per line.
pub const GENERAL_WORDS: &str = include_str!("../data/general.txt");
/// The curator's learnt model (JSON), bundled into the binary.
pub const MODEL_JSON: &str = include_str!("../data/model.json");
/// The names of the features the need heads see, in the order `Curator::dump_to` writes them, and how many
/// of them the older heads see (for fits outside the curator).
pub use learn::{is_request, K_OLD, NAMES as NEED_FEATURES, NOT_REQUEST};

#[cfg(test)]
mod tests;
