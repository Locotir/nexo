//! The hard reply ceiling belongs to the final wire field, not just the preferred size in `ModelRequest`.

use serde_json::Value;

/// An explicit, positive wire override wins when it fits. Missing/malformed values (including negative or zero
/// sentinel values some local servers interpret as unlimited) cannot disable the cap.
pub(crate) fn bounded(value: Option<&Value>, preferred: u32, ceiling: u32) -> u32 {
    let wanted = value.and_then(Value::as_u64).filter(|&n| n > 0).unwrap_or(u64::from(preferred));
    wanted.min(u64::from(ceiling)) as u32
}
