//! Slash commands: the table the palette shows (names, aliases, hints) and
//! how a line is parsed.

mod args;
mod parse;
mod rank;
mod table;
#[cfg(test)]
mod tests;

pub use args::{closed_args, fixed_args, takes_listed_args};
pub use parse::{parse, Command, GoalArg};
pub use rank::{closest, grouped, hits, in_description, rank, Ranked};
pub use table::{is_hidden, lookup, names, Spec, COMMANDS};
