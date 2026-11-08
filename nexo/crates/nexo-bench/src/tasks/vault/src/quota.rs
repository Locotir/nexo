//! How much body text each user may keep.

use std::collections::BTreeMap;

use crate::error::{Result, VaultError};
use crate::users::UserId;

/// A byte budget per user.
#[derive(Debug)]
pub struct Quota {
    limit: usize,
    used: BTreeMap<UserId, usize>,
}

impl Quota {
    /// Every user may keep `limit` bytes.
    pub fn new(limit: usize) -> Quota {
        Quota { limit, used: BTreeMap::new() }
    }

    /// The budget of one user.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The bytes a user has used.
    pub fn used(&self, user: UserId) -> usize {
        self.used.get(&user).copied().unwrap_or(0)
    }

    /// The bytes a user may still add.
    pub fn remaining(&self, user: UserId) -> usize {
        self.limit.saturating_sub(self.used(user))
    }

    /// Uses `bytes` of the budget, or fails and changes nothing.
    pub fn charge(&mut self, user: UserId, bytes: usize) -> Result<()> {
        let total = self.used(user).checked_add(bytes).filter(|t| *t <= self.limit);
        match total {
            Some(t) => {
                self.used.insert(user, t);
                Ok(())
            }
            None => Err(VaultError::QuotaExceeded(format!("user {user} would pass {} bytes", self.limit))),
        }
    }

    /// Gives `bytes` back (never below zero).
    pub fn release(&mut self, user: UserId, bytes: usize) {
        let left = self.used(user).saturating_sub(bytes);
        self.used.insert(user, left);
    }
}
