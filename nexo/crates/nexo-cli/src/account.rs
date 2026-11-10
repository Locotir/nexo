//! Getting a provider working without editing a file: `nexo auth` (keys and
//! subscription logins kept in the credential store), `nexo use` (the
//! default provider and model), `nexo doctor` (what is set up, what is
//! missing, and whether the provider answers) and `nexo guide` (the setup
//! guide, written for a person or an assistant doing it for them).

use std::io::{BufRead, IsTerminal, Read};

use nexo_core::auth::{self, Credential};
use nexo_core::config::{Config, ProviderConfig};
use nexo_providers::login;
use toml_edit::Value;

pub const GUIDE: &str = include_str!("../../../docs/SETUP-FOR-AGENTS.md");

/// How to put a key in the environment, in the shell the person types into.
pub const KEY_IN_ENV: &str =
    if cfg!(windows) { "setx DEEPSEEK_API_KEY \"...\", then a new terminal" } else { "export DEEPSEEK_API_KEY=..." };

/// A provider nexo knows by name and the configuration does not list (a
/// directory entry or a preset alias) is added under that name.
pub fn adopt(cfg: &mut Config, name: &str) -> bool {
    if cfg.providers.contains_key(name) {
        return true;
    }
    match nexo_providers::presets::find(name) {
        Some(mut p) => {
            p.auth.get_or_insert_with(|| name.to_string());
            cfg.providers.insert(name.to_string(), p);
            true
        }
        None => false,
    }
}

/// Where the key of `name` comes from, for listings: `$VAR`, `file`,
/// `command`, `stored api key`, `stored copilot login`; `-` when none is
/// needed, `missing` when one is and none is found.
pub fn key_source(name: &str, p: &ProviderConfig) -> String {
    let set = |v: &str| std::env::var(v).map(|k| !k.trim().is_empty()).unwrap_or(false);
    if let Some(v) = p.api_key_env.as_deref().filter(|v| set(v)) {
        return format!("${v}");
    }
    if p.api_key_file.as_ref().is_some_and(|f| f.exists()) {
        return "file".into();
    }
    if p.api_key_cmd.is_some() && p.resolve_key().is_some() {
        return "command".into();
    }
    if let Some(c) = auth::get(p.auth.as_deref().unwrap_or(name)) {
        return format!("stored {}", c.kind());
    }
    if p.needs_key() {
        "missing".into()
    } else {
        "-".into()
    }
}

/// How to give `name` its key, in one line.
pub fn how_to_unlock(name: &str, p: &ProviderConfig) -> String {
    match (&p.login, &p.api_key_env) {
        (Some(_), Some(v)) => format!("set ${v}, or run `nexo auth login {name}` (subscription)"),
        (Some(_), None) => format!("run `nexo auth login {name}` (subscription login)"),
        (None, Some(v)) => format!("set ${v}, or run `nexo auth login {name} --key`"),
        (None, None) => format!("run `nexo auth login {name} --key`, or set api_key_cmd in the configuration"),
    }
}

/// `nexo auth login <provider> [--key [KEY|-]] [--flow FLOW]`.
pub async fn login_cmd(
    cfg: &mut Config,
    provider: Option<&str>,
    key: Option<&str>,
    flow: Option<&str>,
) -> anyhow::Result<i32> {
    let Some(name) = provider else {
        println!("nexo auth login <provider>          a subscription login, or the key (asked for)");
        println!("nexo auth login <provider> --key K  the key (`--key -` reads it from stdin)\n");
        println!("subscription logins:");
        for (f, what) in login::FLOWS {
            println!("  {f:<9} {what}");
        }
        println!("\nany other provider takes a key: `nexo providers --all` lists every one nexo knows");
        return Ok(1);
    };
    if !adopt(cfg, name) && flow.is_none() && key.is_none() {
        anyhow::bail!("unknown provider {name:?}; `nexo providers --all` lists them, or give --key to keep one anyway");
    }
    let p = cfg.providers.get(name).cloned().unwrap_or_default();
    let entry = p.auth.clone().unwrap_or_else(|| name.to_string());
    let flow = flow.map(str::to_string).or_else(|| if key.is_none() { p.login.clone() } else { None });

    let cred = match flow {
        Some(f) => {
            if !login::is_flow(&f) {
                anyhow::bail!("no login flow {f:?}; there are: {}", flows());
            }
            println!("logging in to {name} ({f})…");
            let show = |d: &login::DeviceCode| {
                if d.code.is_empty() {
                    // the browser flow: the page is opened, and the answer comes back to this machine
                    let opened = login::open_url(&d.url);
                    println!("\n  {}   {}\n", if opened { "opened" } else { "open  " }, d.url);
                } else {
                    println!("\n  open   {}\n  enter  {}\n", d.url, d.code);
                }
                println!("waiting for the browser (Ctrl+C gives up)…");
            };
            login::login(&f, &show).await.map_err(|e| anyhow::anyhow!("{}", e.message))?
        }
        None => Credential::Api { key: read_key(name, &p, key)? },
    };
    if cred.metadata("source") == Some("codex-cli") {
        println!("using the Codex CLI login on this machine (its tokens stay in its file and are renewed there)");
    }
    let kind = cred.kind();
    auth::set(&entry, cred)?;
    println!("{name}: {kind} saved in {}", auth::file().display());
    if cfg.provider.as_deref() != Some(name) {
        println!("make it the default: nexo use {name}");
    }
    Ok(0)
}

fn flows() -> String {
    login::FLOWS.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(", ")
}

fn read_key(name: &str, p: &ProviderConfig, key: Option<&str>) -> anyhow::Result<String> {
    let k = match key {
        Some("-") | Some("") | None => {
            let stdin = std::io::stdin();
            if stdin.is_terminal() {
                let hint = p.api_key_env.as_deref().map(|v| format!(" (the one ${v} would hold)")).unwrap_or_default();
                eprint!("paste the key for {name}{hint}, it stays hidden: ");
                match read_hidden() {
                    Some(line) => {
                        let line = line?;
                        eprintln!("({} characters)", line.trim().chars().count());
                        line
                    }
                    None => {
                        let mut line = String::new();
                        stdin.lock().read_line(&mut line)?;
                        line
                    }
                }
            } else {
                let mut all = String::new();
                stdin.lock().read_to_string(&mut all)?;
                all
            }
        }
        Some(k) => k.to_string(),
    };
    let k = k.trim().to_string();
    if k.is_empty() {
        anyhow::bail!("no key given");
    }
    Ok(k)
}

/// A line typed or pasted at the terminal without showing it: a key on the screen is a key anyone near
/// it, or a screen recording, can read. `None`: the terminal cannot hide it (read it the usual way).
fn read_hidden() -> Option<anyhow::Result<String>> {
    crossterm::terminal::enable_raw_mode().ok()?;
    let line = hidden_line(crossterm::event::read);
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    Some(line)
}

/// The line the events type: up to Enter, Backspace takes a character back, Esc or Ctrl+C give up.
fn hidden_line(mut next: impl FnMut() -> std::io::Result<crossterm::event::Event>) -> anyhow::Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
    let mut line = String::new();
    loop {
        match next()? {
            Event::Paste(s) => line.push_str(&s),
            // Windows reports a key's release too
            Event::Key(k) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => return Ok(line),
                KeyCode::Esc => anyhow::bail!("no key given"),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => anyhow::bail!("no key given"),
                // a shortcut (Ctrl+V in a console that does not paste on it) is not part of the key; Ctrl and
                // Alt together are AltGr on Windows, which types a character (@, #, €)
                KeyCode::Char(_)
                    if k.modifiers.contains(KeyModifiers::CONTROL) != k.modifiers.contains(KeyModifiers::ALT) => {}
                KeyCode::Char(c) => line.push(c),
                KeyCode::Backspace => {
                    line.pop();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// `nexo auth list`: what the store holds, never the secrets.
pub fn list_cmd() -> i32 {
    let all = auth::all();
    println!("store: {}", auth::file().display());
    if all.is_empty() {
        println!("(empty: `nexo auth login <provider>` adds one)");
        return 0;
    }
    let now = auth::now();
    for (name, c) in all {
        let when = match &c {
            Credential::Oauth { expires, refresh, .. } if *expires != 0 => {
                let left = expires.saturating_sub(now);
                let renew = if refresh.is_empty() { "" } else { ", renewed when used" };
                if left == 0 {
                    format!("expired{renew}")
                } else {
                    format!("expires in {}{renew}", human(left))
                }
            }
            _ => String::new(),
        };
        println!("  {name:<16} {:<18} {when}", c.kind());
    }
    0
}

/// `nexo usage`: the plan of every subscription login in the store, as the vendor reports it now.
pub async fn usage_cmd(cfg: &mut Config) -> anyhow::Result<i32> {
    let mut shown = 0;
    let now = auth::now() as i64;
    for (name, cred) in auth::all() {
        if !matches!(cred, Credential::Oauth { .. }) || !adopt(cfg, &name) {
            continue;
        }
        let p = cfg.providers[&name].clone();
        if p.login.as_deref() == Some("xai") {
            println!("SuperGrok (xai): xAI documents no usage endpoint, so there is nothing to read");
            shown += 1;
            continue;
        }
        match nexo_providers::usage::fetch(&name, &p).await {
            Ok(Some(u)) => {
                println!("{}", u.detail(now).join("\n  "));
                shown += 1;
            }
            Ok(None) => {}
            Err(e) => {
                println!("{name}: the plan could not be read ({})", e.message);
                shown += 1;
            }
        }
    }
    if shown == 0 {
        println!("no subscription login stored: `nexo auth login codex` or `nexo auth login copilot` adds one");
        return Ok(1);
    }
    Ok(0)
}

fn human(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `nexo auth logout <provider>`.
pub fn logout_cmd(cfg: &Config, name: &str) -> anyhow::Result<i32> {
    let entry = cfg.providers.get(name).and_then(|p| p.auth.clone()).unwrap_or_else(|| name.to_string());
    if auth::remove(&entry)? {
        println!("{name}: removed from {}", auth::file().display());
        Ok(0)
    } else {
        println!("{name}: nothing stored");
        Ok(1)
    }
}

/// `nexo use <provider> [model]`: written to the user's configuration.
pub fn use_cmd(cfg: &mut Config, name: &str, model: Option<&str>, force: bool) -> anyhow::Result<i32> {
    if !adopt(cfg, name) {
        anyhow::bail!("unknown provider {name:?}; `nexo providers --all` lists them");
    }
    let p = &cfg.providers[name];
    let model = model
        .map(str::to_string)
        .or_else(|| p.default_model.clone())
        .or_else(|| p.models.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("{name} lists no model; give one: nexo use {name} <model>"))?;
    // a name no list knows is most likely a typo: say so, and keep it only when asked to
    let known = known_models(name, p);
    if !force && !known.is_empty() && !known.contains(&model) {
        anyhow::bail!(
            "{model:?} is not a model of {name} that nexo knows; `nexo models {name}` lists them (add --force to use it anyway)"
        );
    }
    let file = nexo_core::paths::user_config_file();
    nexo_agent::config_edit::set(&file, &[], "provider", Value::from(name))?;
    nexo_agent::config_edit::set(&file, &[], "model", Value::from(model.as_str()))?;
    println!("{}: provider = {name}, model = {model}", file.display());
    if !p.ready(name) {
        println!("it has no key yet: {}", how_to_unlock(name, p));
    }
    Ok(0)
}

/// The models nexo knows without asking the vendor: the configured ones and the cached catalogue's.
fn known_models(name: &str, p: &ProviderConfig) -> Vec<String> {
    let mut out: Vec<String> = p.models.clone();
    out.extend(p.default_model.clone());
    if let Some(c) = nexo_providers::catalogue::cached() {
        out.extend(c.model_names(name));
    }
    out
}

/// `nexo doctor [--offline]`: the configuration files, the provider and
/// model a run would use, every provider that is ready, and a live call to
/// the chosen one's model list.
pub async fn doctor(cwd: &std::path::Path, cfg: &Config, offline: bool) -> anyhow::Result<i32> {
    let mark = |ok: bool| if ok { "ok  " } else { "FIX " };
    let user = nexo_core::paths::user_config_file();
    println!("{} user config     {}", mark(true), describe(&user));
    match nexo_core::paths::project_config_file(cwd) {
        Some(p) => println!("{} project config  {}", mark(true), describe(&p)),
        None => println!("{} project config  (none above {})", mark(true), cwd.display()),
    }
    println!("{} credential store {}", mark(true), describe(&auth::file()));
    // keys that do nothing, settings an untrusted project cannot change
    for n in &cfg.notices {
        println!("{} {n}", mark(false));
    }
    if let Some(why) = crate::first_use::unknown_provider(cwd, cfg) {
        println!("{} {why}", mark(false));
        return Ok(1);
    }
    for w in crate::first_use::setting_warnings(cwd, cfg) {
        println!("{} {w}", mark(false));
    }
    // everything the administrator forces, each with its source
    if let Some(p) = &cfg.managed {
        println!("{} managed policy   {}", mark(true), p.file.display());
        for (k, v) in p.summary() {
            println!("     {k} = {v}  (source: {})", nexo_core::managed::SOURCE);
        }
    }

    // a server that needs no key is "ready" only on paper: whether it runs is not checked here
    let ready: Vec<&String> =
        cfg.providers.iter().filter(|(n, p)| p.needs_key() && p.ready(n)).map(|(n, _)| n).collect();
    let local: Vec<&String> = cfg.providers.iter().filter(|(_, p)| !p.needs_key()).map(|(n, _)| n).collect();
    let chose_local = cfg.provider.as_ref().is_some_and(|n| local.contains(&n));
    println!(
        "{} ready providers  {}",
        mark(!ready.is_empty() || chose_local),
        if ready.is_empty() {
            "none".to_string()
        } else {
            ready.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        }
    );
    if !local.is_empty() {
        println!(
            "     local servers (not checked): {}",
            local.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let none = || {
        println!("FIX  no provider chosen and no key found: set one (e.g. {KEY_IN_ENV})");
        println!("     or `nexo auth login <provider>`, then `nexo use <provider>`; `nexo guide` explains every way");
        Ok(1)
    };
    let Some((name, model)) = cfg.pick() else { return none() };
    let p = &cfg.providers[&name];
    // with nothing chosen and no key anywhere, pick falls back to a name: say so instead
    if cfg.provider.is_none() && !p.ready(&name) {
        return none();
    }
    let asked = cfg.provider.as_deref();
    if let Some(a) = asked.filter(|a| *a != name) {
        println!("FIX  provider {a:?} is asked for but cannot be used: {}", unlock_or_unknown(cfg, a));
    }
    println!("{} provider         {name} ({}, {})", mark(true), p.protocol, p.base_url);
    match crate::first_use::unknown_model(cfg) {
        Some(w) => println!("warn model            {w}"),
        None => println!("{} model            {model}", mark(true)),
    }
    let src = key_source(&name, p);
    println!("{} key              {src}", mark(src != "missing"));
    if src == "missing" {
        println!("    {}", how_to_unlock(&name, p));
        return Ok(1);
    }
    let l = nexo_agent::limits::resolve(&name, p, &model, None);
    let n = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into());
    println!(
        "{} limits           context {} · output {} · prices {}",
        mark(true),
        n(l.context_window),
        n(l.max_output),
        l.pricing.map(|p| format!("in {} out {} per million", p.input, p.output)).unwrap_or_else(|| "unknown".into())
    );
    if offline {
        return Ok(0);
    }
    let client = nexo_providers::Client::new(nexo_providers::registry());
    match tokio::time::timeout(std::time::Duration::from_secs(20), client.list_models(p)).await {
        Ok(Ok(list)) => {
            let listed = list.is_empty() || list.iter().any(|m| m.id == model);
            println!("{} live check       the provider answered with {} model(s)", mark(true), list.len());
            if !listed {
                println!("FIX  model {model:?} is not in its list; `nexo models {name}` shows the ones it has");
                return Ok(1);
            }
            Ok(0)
        }
        Ok(Err(e)) => {
            println!("FIX  live check       {}", e.message);
            Ok(1)
        }
        Err(_) => {
            println!("FIX  live check       no answer in 20 s");
            Ok(1)
        }
    }
}

fn unlock_or_unknown(cfg: &Config, name: &str) -> String {
    match cfg.providers.get(name) {
        Some(p) => how_to_unlock(name, p),
        None => "unknown name (`nexo providers --all`)".into(),
    }
}

fn describe(p: &std::path::Path) -> String {
    format!("{}{}", p.display(), if p.exists() { "" } else { " (not created)" })
}

/// Directory entries whose key is in the environment or in the store join
/// the providers, under their own id; presets and their aliases are not
/// repeated.
pub fn add_directory(cfg: &mut Config) {
    let stored = auth::all();
    let presets = nexo_providers::presets::names();
    for e in nexo_providers::directory::all() {
        if presets.contains(&e.id)
            || nexo_providers::directory::alias(e.id).is_some()
            || cfg.providers.contains_key(e.id)
        {
            continue;
        }
        let env = std::env::var(e.key_env).map(|k| !k.trim().is_empty()).unwrap_or(false);
        if env || stored.contains_key(e.id) {
            cfg.providers.insert(e.id.to_string(), e.to_config());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_key_is_what_was_typed_or_pasted_up_to_enter() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let key = |c: KeyCode| Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
        let run = |evs: Vec<Event>| {
            let mut it = evs.into_iter();
            hidden_line(move || it.next().ok_or_else(|| std::io::Error::other("no more")))
        };
        let mut release = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        let typed = vec![
            Event::Paste("key-ab".into()),
            key(KeyCode::Char('x')),
            key(KeyCode::Backspace),
            Event::Key(release),
            key(KeyCode::Char('c')),
            key(KeyCode::Enter),
            key(KeyCode::Char('q')),
        ];
        assert_eq!(run(typed).unwrap(), "key-abc");
        assert!(run(vec![key(KeyCode::Char('a')), key(KeyCode::Esc)]).is_err());
        let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(run(vec![key(KeyCode::Char('a')), ctrl_c]).is_err());
        let with = |c: char, m: KeyModifiers| Event::Key(KeyEvent::new(KeyCode::Char(c), m));
        let shortcut_and_altgr = vec![
            with('a', KeyModifiers::NONE),
            with('v', KeyModifiers::CONTROL),
            with('x', KeyModifiers::ALT),
            with('@', KeyModifiers::CONTROL | KeyModifiers::ALT),
            key(KeyCode::Enter),
        ];
        assert_eq!(run(shortcut_and_altgr).unwrap(), "a@");
    }

    #[test]
    fn a_known_name_is_adopted_and_an_unknown_one_is_not() {
        let mut cfg = Config::default();
        assert!(adopt(&mut cfg, "chutes"));
        assert_eq!(cfg.providers["chutes"].auth.as_deref(), Some("chutes"));
        assert!(adopt(&mut cfg, "google"), "an alias of a preset");
        assert_eq!(cfg.providers["google"].protocol, "gemini");
        assert!(!adopt(&mut cfg, "no-such-vendor"));
    }

    #[test]
    fn the_hint_names_the_login_or_the_variable() {
        let mut p = ProviderConfig::new("openai-chat", "https://x", Some("X_KEY"));
        assert!(how_to_unlock("x", &p).contains("$X_KEY"));
        p.login = Some("xai".into());
        assert!(how_to_unlock("x", &p).contains("nexo auth login x"));
        p.api_key_env = None;
        assert!(how_to_unlock("x", &p).contains("subscription"));
    }

    #[test]
    fn the_guide_covers_every_command_and_login() {
        for s in [
            "nexo auth login",
            "nexo use",
            "nexo doctor",
            "nexo update",
            "nexo providers --all",
            "api_key_cmd",
            "copilot",
            "codex",
        ] {
            assert!(GUIDE.contains(s), "{s}");
        }
    }
}
