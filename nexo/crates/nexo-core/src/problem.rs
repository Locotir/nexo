//! A failure as the person reads it: what failed in plain words, the likely cause, what to do next,
//! and the provider's own text kept apart as the detail. Built from a [`ProviderError`] and what the
//! session knows (provider, model, address, key source), or by hand for the other errors.

#[cfg(test)]
use crate::error::{ErrorKind, ProviderError};
#[cfg(test)]
use std::time::Duration;

mod build;
mod causes;
mod text;

pub use build::*;
pub use causes::says_no_credit;
pub use text::*;

#[cfg(test)]
mod tests;
