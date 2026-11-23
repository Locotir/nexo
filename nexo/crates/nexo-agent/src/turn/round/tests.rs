use super::*;

#[test]
fn prepared_snapshot_tracks_round_changes_without_replacing_prompt() {
    let mut req = ModelRequest::new("model", "system", vec![]);
    let mut snapshot = Some(req.clone());
    req.temperature = Some(0.25);
    req.reasoning = Some(ReasoningConfig::Off);
    req.max_tokens = 123;
    sync_prepared(&mut snapshot, &req);
    let saved = snapshot.unwrap();
    assert_eq!(saved.system, "system");
    assert_eq!(saved.temperature, req.temperature);
    assert_eq!(saved.reasoning, req.reasoning);
    assert_eq!(saved.max_tokens, 123);
}

fn pick(levels: &[&str]) -> Option<ReasoningConfig> {
    calm_level(&levels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), true)
}

fn pick_no_off(levels: &[&str]) -> Option<ReasoningConfig> {
    calm_level(&levels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), false)
}

/// Where off changes nothing in the request (an OpenAI protocol) or thinks more (Opus 5.5), it is not the
/// calm level: the next lowest is, and a model with only off and on gets none, so its asks run free.
#[test]
fn off_is_the_calm_level_only_where_it_thinks_less() {
    let low = Some(ReasoningConfig::Effort(Effort::Low));
    assert_eq!(pick_no_off(&["off", "low", "medium", "high"]), low);
    assert_eq!(pick_no_off(&["none", "minimal", "low"]), Some(ReasoningConfig::Effort(Effort::Minimal)));
    assert_eq!(pick_no_off(&["off", "on"]), None);
    assert_eq!(pick(&["off", "on"]), Some(ReasoningConfig::Off));
}

#[test]
fn the_calm_level_is_the_least_the_model_lists() {
    let (off, min, low) = (
        Some(ReasoningConfig::Off),
        Some(ReasoningConfig::Effort(Effort::Minimal)),
        Some(ReasoningConfig::Effort(Effort::Low)),
    );
    assert_eq!(pick(&["minimal", "low", "medium", "high"]), min);
    assert_eq!(pick(&["off", "low", "high"]), off);
    assert_eq!(pick(&["high", "none", "minimal"]), off);
    assert_eq!(pick(&["low", "medium", "high"]), low);
    // nothing below medium, or no level at all: none is sent
    assert_eq!(pick(&["medium", "high"]), None);
    assert_eq!(pick(&[]), None);
}
