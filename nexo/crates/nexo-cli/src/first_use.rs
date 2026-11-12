//! Checks of the settings a newcomer is likely to get wrong, so the message names the file, the key and the value.

use std::path::{Path, PathBuf};

use nexo_core::Config;

/// The file that sets `key` (a top-level or `table.key` path): the project's, else the user's.
fn source_of(cwd: &Path, table: Option<&str>, key: &str) -> PathBuf {
    let has = |p: &Path| {
        let Ok(text) = std::fs::read_to_string(p) else { return false };
        let Ok(v) = text.parse::<toml_edit::DocumentMut>() else { return false };
        match table {
            Some(t) => v.get(t).and_then(|t| t.get(key)).is_some(),
            None => v.get(key).is_some(),
        }
    };
    if let Some(p) = nexo_core::paths::project_config_file(cwd).filter(|p| has(p)) {
        return p;
    }
    nexo_core::paths::user_config_file()
}

pub(crate) fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// A sentence naming the closest of `known` to `value` (within two edits), else all of them.
fn did_you_mean(value: &str, known: &[String]) -> String {
    match known.iter().min_by_key(|k| distance(&value.to_lowercase(), k)) {
        Some(k) if distance(&value.to_lowercase(), k) <= 2 => format!("did you mean {k:?}?"),
        _ => format!("known providers: {}", known.join(", ")),
    }
}

/// A one-word request that is a typo of a command (`nexo sesions`): the line to print instead of sending it
/// to the model. Words under four letters and anything longer than one word stay a request.
pub fn mistyped_command(word: &str, commands: &[String]) -> Option<String> {
    let w = word.trim().to_lowercase();
    if w.chars().count() < 4 || w.contains(char::is_whitespace) {
        return None;
    }
    let best = commands.iter().filter(|c| c.as_str() != "help").min_by_key(|c| distance(&w, c))?;
    (distance(&w, best) <= 2).then(|| {
        format!("{word:?} is not a command; did you mean `nexo {best}`? (to send it as a request: `nexo -p {word}`)")
    })
}

/// `provider = "x"` in the configuration when nothing by that name exists.
pub fn unknown_provider(cwd: &Path, cfg: &Config) -> Option<String> {
    let name = cfg.provider.as_deref()?;
    if cfg.providers.contains_key(name) {
        return None;
    }
    let mut known: Vec<String> = cfg.providers.keys().cloned().collect();
    known.extend(nexo_providers::presets::names().into_iter().map(String::from));
    known.sort();
    known.dedup();
    Some(format!(
        "{}: provider = {name:?} is not a known provider ({}); `nexo providers` lists them",
        source_of(cwd, None, "provider").display(),
        did_you_mean(name, &known)
    ))
}

/// One line for each setting that cannot work: an unknown theme or context policy.
pub fn setting_warnings(cwd: &Path, cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    let theme = cfg.ui.theme.trim();
    if !theme.eq_ignore_ascii_case("auto") && nexo_tui::theme::Theme::by_name(theme).is_none() {
        out.push(format!(
            "{}: [ui] theme = {theme:?} is not a theme; accepted: auto, nexo, dark, light, mono, catppuccin-mocha, gruvbox, nord, dracula, solarized-dark, tokyo-night",
            source_of(cwd, Some("ui"), "theme").display()
        ));
    }
    let policy = cfg.context.policy.trim();
    if !matches!(policy, "auto" | "plain" | "curator" | "compact" | "") {
        out.push(format!(
            "{}: [context] policy = {policy:?} is not a policy; accepted: auto, plain, curator, compact",
            source_of(cwd, Some("context"), "policy").display()
        ));
    }
    out
}

/// The `model` setting, when the provider's own list and the price catalogue both lack it: a warning,
/// since a provider may take names it does not list. Only a hosted provider with a list of models is
/// judged: a local server holds whatever the person pulled, which only the live check can see.
pub fn unknown_model(cfg: &Config) -> Option<String> {
    let model = cfg.model.as_deref()?;
    let (name, _) = cfg.pick()?;
    let p = cfg.providers.get(&name)?;
    if !p.needs_key()
        || p.models.is_empty()
        || p.models.iter().any(|m| m == model)
        || p.default_model.as_deref() == Some(model)
    {
        return None;
    }
    if nexo_providers::catalogue::lookup(&name, model).is_some() {
        return None;
    }
    Some(format!(
        "{model:?} is not in the model list of {name} ({}). Pick one with /model or `nexo models {name}`; if {name} takes names it does not list, this is fine",
        p.models.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_closest_name_is_offered_only_when_it_is_near() {
        let known = vec!["anthropic".to_string(), "ollama".to_string()];
        assert_eq!(did_you_mean("anthropc", &known), "did you mean \"anthropic\"?");
        assert!(did_you_mean("nosuch", &known).starts_with("known providers: anthropic, ollama"));
    }

    #[test]
    fn a_model_the_provider_does_not_list_is_a_warning_and_no_list_is_none() {
        let mut p = nexo_core::config::ProviderConfig::new("openai-chat", "https://x", Some("X_KEY"));
        p.models = vec!["listed".into()];
        let mut cfg = Config::default();
        cfg.providers.insert("x".into(), p);
        cfg.provider = Some("x".into());
        cfg.model = Some("listed".into());
        assert!(unknown_model(&cfg).is_none());
        cfg.model = Some("nosuchmodel".into());
        let w = unknown_model(&cfg).unwrap();
        assert!(w.contains("not in the model list of x") && w.contains("/model"), "{w}");
        cfg.providers.get_mut("x").unwrap().models.clear();
        assert!(unknown_model(&cfg).is_none(), "a provider with no list is not judged");
    }

    #[test]
    fn one_word_near_a_command_is_a_typo_and_nothing_else_is() {
        let cmds: Vec<String> = ["sessions", "doctor", "use", "review"].map(String::from).to_vec();
        assert!(mistyped_command("sesions", &cmds).unwrap().contains("`nexo sessions`"));
        assert!(mistyped_command("Docter", &cmds).is_some());
        assert!(mistyped_command("ls", &cmds).is_none(), "too short to judge");
        assert!(mistyped_command("refactor", &cmds).is_none());
        assert!(mistyped_command("fix the bug", &cmds).is_none());
    }
}
