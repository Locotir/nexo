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

pub(super) fn family(model: &str) -> Option<&'static str> {
    family_version(model).map(|f| f.0)
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

/// Reasoning off is a request for less thinking: before 5.5 it is no thinking, and on 5.5 Sonnet has a lowest
/// setting; the other 5.5 models take nothing for it and think at the server's default effort.
pub fn off_thinks_less(model: &str) -> bool {
    !modern(model) || family(model) == Some("sonnet")
}
