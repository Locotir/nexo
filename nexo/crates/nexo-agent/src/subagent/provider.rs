use super::*;

/// The models a provider lists: its default and the others set up for it.
pub(super) fn listed(cfg: &ProviderConfig) -> impl Iterator<Item = &str> {
    cfg.default_model.iter().chain(cfg.models.iter()).map(String::as_str)
}

/// Where a model asked for without a provider runs: `Ok(None)` on the conversation's provider (it lists
/// the model, or nobody does), `Ok(Some(name))` on the one other provider that lists it, `Err(names)`
/// when several others do. Names match exactly, ignoring ASCII case.
pub(super) fn provider_of<'a>(
    model: &str,
    main: (&str, &ProviderConfig),
    providers: &'a std::collections::BTreeMap<String, ProviderConfig>,
) -> Result<Option<&'a str>, Vec<&'a str>> {
    if listed(main.1).any(|m| m.eq_ignore_ascii_case(model)) {
        return Ok(None);
    }
    let on: Vec<&str> = providers
        .iter()
        .filter(|(n, c)| n.as_str() != main.0 && listed(c).any(|m| m.eq_ignore_ascii_case(model)))
        .map(|(n, _)| n.as_str())
        .collect();
    match on.len() {
        0 => Ok(None),
        1 => Ok(Some(on[0])),
        _ => Err(on),
    }
}
