//! Which Claude a model name is, and what that model takes.

/// The family (`sonnet`, `opus`, `haiku`, `fable`, `mythos`) and version of a name like `claude-sonnet-5-5`, also as a
/// cloud id (`anthropic.claude-sonnet-5-5-v1:0`, `claude-sonnet-5-5@20260101`).
pub(super) fn family_version(model: &str) -> Option<(&'static str, u32, u32)> {
    let m = model.to_ascii_lowercase();
    let rest = &m[m.find("claude-")? + "claude-".len()..];
    let mut t = rest.split(['-', '.', '@', ':', '[', ']']);
    let fam = match t.next()? {
        "sonnet" => "sonnet",
        "opus" => "opus",
        "haiku" => "haiku",
        "fable" => "fable",
        "mythos" => "mythos",
        _ => return None,
    };
    let major: u32 = t.next()?.parse().ok()?;
    // a date after the major (`claude-sonnet-5-20260101`) is not a minor version
    let minor: u32 = t.next().filter(|s| s.len() <= 2).and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((fam, major, minor))
}

/// Claude 5.5 and later: adaptive thinking, effort, no sampling parameters.
pub fn modern(model: &str) -> bool {
    family_version(model).is_some_and(|(_, major, minor)| (major, minor) >= (5, 5))
}

/// Models that take `thinking: adaptive` plus `output_config.effort`, and answer a manual `budget_tokens` with a
/// 400: Fable, Mythos, Opus and Sonnet from 4.6, Haiku from 5. Any other id (Haiku 4.5, Sonnet 4.5, Opus 4.5 and
/// older, `claude-3*`, another vendor's model) still takes `budget_tokens`.
pub(super) fn adaptive_thinking(model: &str) -> bool {
    match family_version(model) {
        Some(("fable" | "mythos", ..)) => true,
        Some(("opus" | "sonnet", major, minor)) => (major, minor) >= (4, 6),
        Some(("haiku", major, _)) => major >= 5,
        _ => false,
    }
}

/// Models that answer a `temperature` with a 400 (or any non-default value of it): Fable, Mythos, Opus from 4.7,
/// Sonnet and Haiku from 5. Every other id, Claude or not, still takes it.
pub(super) fn takes_temperature(model: &str) -> bool {
    match family_version(model) {
        Some(("fable" | "mythos", ..)) => false,
        Some(("opus", major, minor)) => (major, minor) < (4, 7),
        Some(("sonnet" | "haiku", major, _)) => major < 5,
        _ => true,
    }
}

/// Models that take `output_config.effort: xhigh` (between `high` and `max`): Opus from 4.7, Sonnet, Haiku, Fable and
/// Mythos from 5. Older ones stop at `high`, with `max` above it where they take effort at all.
pub(super) fn takes_xhigh(model: &str) -> bool {
    match family_version(model) {
        Some(("fable" | "mythos", ..)) => true,
        Some(("opus", major, minor)) => (major, minor) >= (4, 7),
        Some(("sonnet" | "haiku", major, _)) => major >= 5,
        _ => false,
    }
}

/// The reasoning levels a Claude model takes: a thinking budget (`low` to `high`) before 4.6, adaptive effort
/// above it with `max` and, where the model has it, `xhigh`. Any other id keeps the budget levels.
pub fn effort_levels(model: &str) -> &'static [&'static str] {
    if !adaptive_thinking(model) {
        &["off", "low", "medium", "high"]
    } else if takes_xhigh(model) {
        &["off", "low", "medium", "high", "xhigh", "max"]
    } else {
        &["off", "low", "medium", "high", "max"]
    }
}

/// What reasoning off sends as `thinking`: Sonnet 5.5 its lowest setting, and `disabled` where Claude 5 and later
/// run adaptive thinking when it is left out (Opus before 5.5, Sonnet before 5.5, Haiku) and accept it. None: the
/// field is left out, which is off before Claude 5, and no way to turn it off on Fable, Mythos and Opus 5.5.
pub(super) fn off_thinking(model: &str) -> Option<&'static str> {
    match family_version(model) {
        Some(("sonnet", major, minor)) if (major, minor) >= (5, 5) => Some("between_tools"),
        Some(("opus" | "sonnet", major, minor)) if major >= 5 && (major, minor) < (5, 5) => Some("disabled"),
        Some(("haiku", major, _)) if major >= 5 => Some("disabled"),
        _ => None,
    }
}

/// Reasoning off is a request for less thinking: before 5 it is no thinking (the field is left out), from 5 on
/// where `off_thinking` names a setting; Fable, Mythos and Opus 5.5 take nothing for it and think at the server's
/// default effort.
pub fn off_thinks_less(model: &str) -> bool {
    match family_version(model) {
        Some(("fable" | "mythos", ..)) => false,
        Some(("opus", major, minor)) => (major, minor) < (5, 5),
        _ => true,
    }
}
