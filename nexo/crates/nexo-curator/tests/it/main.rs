//! One test binary for the crate: one `mod` per former `tests/*.rs` file.

mod added;
mod golden;
mod h1;
mod ledger;
mod notes;
mod pockets;
mod rewrite;
mod rules;
mod scale;
#[path = "../support/scale_gen/mod.rs"]
mod scale_gen;
mod sim_behave;
mod sim_events;
mod sim_family;
mod sim_fit;
mod sim_search;
mod sim_unsent;
mod standing;
mod stress;
mod structured_facts;
mod topic_switch;
mod window;
