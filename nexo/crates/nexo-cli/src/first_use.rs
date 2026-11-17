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

/// The warning for a setting whose value is a word from a fixed list that the runtime reads leniently
/// (anything else becomes the default without a message). The comparison ignores case and padding,
/// as the readers do, unless `exact` is set.
fn word_warning(source: &Path, table: &str, key: &str, value: &str, accepted: &[&str], exact: bool) -> Option<String> {
    let ok =
        if exact { accepted.contains(&value) } else { accepted.iter().any(|a| a.eq_ignore_ascii_case(value.trim())) };
    (!ok).then(|| {
        format!(
            "{}: [{table}] {key} = {value:?} is not a known value; accepted: {}",
            source.display(),
            accepted.join(", ")
        )
    })
}

/// The string at `table.key` in the first of the project's and the person's files that sets it
/// (the order `source_of` uses), with that file. `OnLimit`, `Detail` and `Figure` turn an unknown word
/// into their default while the file is read, so only the file still holds what was written.
fn raw_value(cwd: &Path, table: &str, key: &str) -> Option<(PathBuf, String)> {
    let read = |p: PathBuf| {
        let text = std::fs::read_to_string(&p).ok()?;
        let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
        let item = doc.get(table)?.get(key)?;
        Some((p, item.as_str().map(str::to_string)))
    };
    let (path, value) = nexo_core::paths::project_config_file(cwd)
        .and_then(read)
        .or_else(|| read(nexo_core::paths::user_config_file()))?;
    value.map(|v| (path, v))
}

/// The first of `files` whose `[agent] key` is a number that limits nothing (0, negative or nan). An untrusted
/// project's higher limit is replaced by the person's own while the files load, so the value that now
/// reads as no limit can sit in the person's file while the project file also sets the key.
fn file_with_empty_limit(files: &[PathBuf], key: &str) -> Option<PathBuf> {
    files.iter().find_map(|p| {
        let text = std::fs::read_to_string(p).ok()?;
        let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
        let item = doc.get("agent")?.get(key)?;
        let v = item.as_float().or_else(|| item.as_integer().map(|i| i as f64))?;
        (v.is_nan() || v <= 0.0).then(|| p.clone())
    })
}

/// One line for each setting that cannot work or that reads as something else than it says: a word outside
/// its list (theme, context policy, mail inbound, screen, status pieces, brief engine, on_limit, detail, figure),
/// an unknown web search backend, and a spend limit that limits nothing.
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
    let mail = cfg.mail.inbound.trim().to_ascii_lowercase();
    // An unknown word reads as `auto`, the most permissive of the four, so a typo of `refuse` opens the inbox.
    if !matches!(mail.as_str(), "auto" | "accept" | "hold" | "refuse") {
        out.push(format!(
            "{}: [mail] inbound = {:?} is not a mode, so it reads as auto; accepted: auto, accept, hold, refuse",
            source_of(cwd, Some("mail"), "inbound").display(),
            cfg.mail.inbound
        ));
    }
    // The runtime compares this one exactly, so ` inline` or `Inline` gives the full screen.
    out.extend(word_warning(
        &source_of(cwd, Some("ui"), "screen"),
        "ui",
        "screen",
        &cfg.ui.screen,
        &["full", "inline"],
        true,
    ));
    // Unknown pieces are dropped, and a list with none known left draws an empty status bar.
    let unknown: Vec<String> = cfg
        .ui
        .status_line
        .iter()
        .filter(|n| !nexo_tui::ui::STATUS_PIECES.iter().any(|p| p.eq_ignore_ascii_case(n.trim())))
        .map(|n| format!("{n:?}"))
        .collect();
    if !unknown.is_empty() {
        out.push(format!(
            "{}: [ui] status_line has unknown pieces {}, which are dropped (a list with none known draws an empty bar); accepted: {}",
            source_of(cwd, Some("ui"), "status_line").display(),
            unknown.join(", "),
            nexo_tui::ui::STATUS_PIECES.join(", ")
        ));
    }
    let engine = cfg.agent.subagent_brief_engine.trim().to_ascii_lowercase();
    if !matches!(engine.as_str(), "auto" | "curator" | "lexical" | "rust") {
        out.push(format!(
            "{}: [agent] subagent_brief_engine = {:?} is not an engine, so it reads as auto; accepted: auto, curator, lexical, rust",
            source_of(cwd, Some("agent"), "subagent_brief_engine").display(),
            cfg.agent.subagent_brief_engine
        ));
    }
    let search = cfg.web.search.trim();
    // The runtime refuses an unknown backend only at the first search, as a tool failure.
    if !search.is_empty() && !search.eq_ignore_ascii_case("auto") && nexo_tools::web::Backend::parse(search).is_none() {
        out.push(format!(
            "{}: [web] search = {search:?} is not a backend; accepted: auto, duckduckgo (ddg), brave, tavily, exa, searxng (searx)",
            source_of(cwd, Some("web"), "search").display()
        ));
    }
    // A limit at or under zero, or nan, is no limit at all: `max_cost = 0` does not mean "spend nothing".
    let files: Vec<PathBuf> =
        nexo_core::paths::project_config_file(cwd).into_iter().chain([nexo_core::paths::user_config_file()]).collect();
    for (key, v) in [("max_cost", cfg.agent.max_cost), ("max_cost_day", cfg.agent.max_cost_day)] {
        if let Some(v) = v.filter(|v| v.is_nan() || *v <= 0.0) {
            let file = file_with_empty_limit(&files, key).unwrap_or_else(|| source_of(cwd, Some("agent"), key));
            out.push(format!(
                "{}: [agent] {key} = {v} does not stop anything: a limit that is 0, negative or nan means no limit; use a positive dollar amount, or remove the key",
                file.display()
            ));
        }
    }
    // These three read an unknown word as their default while the file loads, so `cfg` cannot tell.
    for (table, key, accepted) in [
        ("agent", "on_limit", &["checkpoint", "ask"][..]),
        ("ui", "detail", &["quiet", "full"][..]),
        ("ui", "figure", &["galaxy", "constellation", "none"][..]),
    ] {
        if let Some((path, v)) = raw_value(cwd, table, key) {
            out.extend(word_warning(&path, table, key, &v, accepted, false));
        }
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

    /// The warnings of `cfg` that name `key`, with a project folder that holds no settings of its own.
    fn warnings_for(cfg: &Config, key: &str) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        setting_warnings(dir.path(), cfg).into_iter().filter(|w| w.contains(key)).collect()
    }

    #[test]
    fn a_mistyped_mail_inbound_mode_is_named_and_a_valid_one_is_not() {
        let mut cfg = Config::default();
        cfg.mail.inbound = "refues".into();
        let w = warnings_for(&cfg, "inbound");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"refues\"") && w[0].contains("accepted: auto, accept, hold, refuse"), "{w:?}");
        cfg.mail.inbound = " Refuse ".into();
        assert!(warnings_for(&cfg, "inbound").is_empty());
    }

    #[test]
    fn a_screen_other_than_full_or_inline_is_named_because_the_runtime_compares_it_exactly() {
        let mut cfg = Config::default();
        cfg.ui.screen = "Inline".into();
        let w = warnings_for(&cfg, "screen");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"Inline\"") && w[0].contains("accepted: full, inline"), "{w:?}");
        cfg.ui.screen = "inline".into();
        assert!(warnings_for(&cfg, "screen").is_empty());
    }

    #[test]
    fn unknown_status_pieces_are_listed_and_known_ones_in_any_case_are_not() {
        let mut cfg = Config::default();
        cfg.ui.status_line = vec!["Model".into(), "modle".into(), " cost ".into(), "dirr".into()];
        let w = warnings_for(&cfg, "status_line");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"modle\", \"dirr\"") && w[0].contains("accepted: model, effort"), "{w:?}");
        cfg.ui.status_line = vec!["Model".into(), " cost ".into()];
        assert!(warnings_for(&cfg, "status_line").is_empty());
    }

    #[test]
    fn a_mistyped_subagent_brief_engine_is_named_and_a_valid_one_is_not() {
        let mut cfg = Config::default();
        cfg.agent.subagent_brief_engine = "lexicall".into();
        let w = warnings_for(&cfg, "subagent_brief_engine");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"lexicall\"") && w[0].contains("accepted: auto, curator, lexical, rust"), "{w:?}");
        cfg.agent.subagent_brief_engine = "Rust".into();
        assert!(warnings_for(&cfg, "subagent_brief_engine").is_empty());
    }

    #[test]
    fn an_unknown_web_search_backend_is_named_before_the_first_search() {
        let mut cfg = Config::default();
        cfg.web.search = "tavilly".into();
        let w = warnings_for(&cfg, "[web]");
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"tavilly\"") && w[0].contains("accepted: auto, duckduckgo (ddg)"), "{w:?}");
        for ok in ["", "AUTO", " Tavily ", "ddg", "searx"] {
            cfg.web.search = ok.into();
            assert!(warnings_for(&cfg, "[web]").is_empty(), "{ok:?}");
        }
    }

    #[test]
    fn a_spend_limit_that_is_zero_negative_or_nan_is_said_to_limit_nothing() {
        for key in ["max_cost", "max_cost_day"] {
            for bad in [0.0, -1.0, f64::NAN] {
                let mut cfg = Config::default();
                if key == "max_cost" {
                    cfg.agent.max_cost = Some(bad);
                } else {
                    cfg.agent.max_cost_day = Some(bad);
                }
                let w = warnings_for(&cfg, key);
                assert_eq!(w.len(), 1, "{key} = {bad}: {w:?}");
                assert!(w[0].contains("means no limit"), "{w:?}");
            }
            let mut cfg = Config::default();
            if key == "max_cost" {
                cfg.agent.max_cost = Some(1.5);
            } else {
                cfg.agent.max_cost_day = Some(1.5);
            }
            assert!(warnings_for(&cfg, key).is_empty(), "{key} = 1.5");
        }
    }

    #[test]
    fn an_empty_spend_limit_is_pinned_on_the_file_that_holds_it_not_on_one_that_was_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        let user = dir.path().join("user.toml");
        std::fs::write(&project, "[agent]\nmax_cost = 5\nmax_cost_day = 0\n").unwrap();
        std::fs::write(&user, "[agent]\nmax_cost = 0.0\nmax_cost_day = nan\n").unwrap();
        let files = [project.clone(), user.clone()];
        assert_eq!(file_with_empty_limit(&files, "max_cost"), Some(user.clone()), "the project's 5 is a real limit");
        assert_eq!(file_with_empty_limit(&files, "max_cost_day"), Some(project.clone()), "both are empty: the first");
        std::fs::write(&user, "[agent]\nmax_cost = 2\n").unwrap();
        assert_eq!(file_with_empty_limit(&files, "max_cost"), None);
        assert_eq!(file_with_empty_limit(&files, "other"), None);
    }

    #[test]
    fn words_the_loader_turns_into_a_default_are_found_in_the_project_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::create_dir(dir.path().join(".nexo")).unwrap();
        std::fs::write(
            dir.path().join(".nexo/config.toml"),
            "[agent]\non_limit = \"asck\"\n[ui]\ndetail = \"Full\"\nfigure = \"gallaxy\"\n",
        )
        .unwrap();
        let w = setting_warnings(dir.path(), &Config::default());
        let has = |key: &str, word: &str| w.iter().any(|l| l.contains(key) && l.contains(word));
        assert!(has("on_limit", "\"asck\"") && has("figure", "\"gallaxy\""), "{w:?}");
        assert!(w.iter().all(|l| !l.contains("detail")), "a valid word in another case is fine: {w:?}");
        assert!(w.iter().any(|l| l.contains("accepted: checkpoint, ask")), "{w:?}");
    }

    #[test]
    fn a_word_outside_its_list_is_accepted_only_by_the_comparison_the_reader_makes() {
        let p = Path::new("c.toml");
        assert!(word_warning(p, "ui", "figure", " NONE ", &["galaxy", "none"], false).is_none());
        assert!(word_warning(p, "ui", "screen", " inline", &["full", "inline"], true).is_some());
    }
}
