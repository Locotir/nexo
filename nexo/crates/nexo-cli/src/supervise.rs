//! `nexo supervise`: the daemon that watches long-running sessions, the hooks those sessions call, and the
//! text a person adds to their settings to install the hooks. Nothing here edits a settings file.

use std::io::Read;

use nexo_agent::supervise::{
    install,
    run::Setup,
    socket::{self, HookKind, Request, HOOK_TIMEOUT},
    NexoSource, Options, Panel, Policy,
};

const MAX_STDIN: u64 = 1 << 20;

/// What a hook prints: the supervisor's answer, or `{}` when nobody answers. Always exits 0, so a
/// supervisor that is down, slow or confused never holds a session back.
pub fn hook(kind: &str) -> i32 {
    let mut raw = String::new();
    let _ = std::io::stdin().take(MAX_STDIN).read_to_string(&mut raw);
    println!("{}", hook_output(kind, &raw, &socket::default_path()));
    0
}

/// The line a hook prints for this input, asking the supervisor at `sock`.
pub fn hook_output(kind: &str, input: &str, sock: &std::path::Path) -> String {
    let none = "{}".to_string();
    let Some(kind) = HookKind::parse(kind) else { return none };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(input) else { return none };
    match socket::ask(sock, &Request::Hook { kind, payload }, HOOK_TIMEOUT) {
        Some(r) if r.ok && r.out.is_object() => r.out.to_string(),
        _ => none,
    }
}

/// Run the supervisor until interrupted.
pub async fn daemon() -> anyhow::Result<i32> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let user = nexo_core::paths::config_dir().join("supervisor.toml");
    let project = cwd.join(".nexo").join("supervisor.toml");
    let loaded = Policy::load(Some(&user), Some(&project));
    for e in &loaded.errors {
        eprintln!("nexo: supervisor policy: {e}");
    }
    if !loaded.policy.enabled {
        eprintln!("nexo: the supervisor is switched off (enabled = false in {})", user.display());
        return Ok(0);
    }
    let setup = Setup::detect(&loaded.policy);
    let opts = Options { must_listen: true, ..Options::detect() };
    let model = loaded.policy.model.clone();
    let panel = match Panel::start_with(&setup, loaded.policy, NexoSource::default(), &opts) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("nexo: a supervisor is already running");
            return Ok(1);
        }
        Err(e) => return Err(e.into()),
    };
    panel.add_source(Box::new(nexo_agent::supervise::source_mail::MailSource::new(nexo_core::mailbox::mail_root())));
    let mut notes: Vec<String> = loaded.errors.clone();
    if let Some(m) = &model {
        // the providers come from the config of the folder the daemon starts in
        let providers: Vec<(String, nexo_core::config::ProviderConfig)> = match nexo_core::Config::load(&cwd) {
            Ok(c) => c.providers.iter().map(|(n, p)| (n.clone(), p.clone())).collect(),
            Err(e) => {
                notes.push(format!("config: {e}"));
                Vec::new()
            }
        };
        let client = nexo_agent::supervise::model::default_client();
        match nexo_agent::supervise::model::judge_for(m, &providers, client, tokio::runtime::Handle::current()) {
            Ok(j) => panel.set_judge(j),
            Err(e) => notes.push(e),
        }
    }
    for n in &notes {
        eprintln!("nexo: supervisor: {n}");
    }
    panel.set_notes(notes);
    eprintln!("nexo: supervising; hooks reach it at {}", socket::default_path().display());
    tokio::signal::ctrl_c().await?;
    drop(panel);
    Ok(0)
}

/// The settings text to add. Prints; never writes.
pub fn install_print() -> i32 {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "nexo".into());
    println!("{}", install::snippet(&exe));
    eprintln!(
        "\nAdd the \"hooks\" entries above to the settings file of the sessions you want watched (the user file \
         covers all of them, a project file one folder). nexo does not edit it. Then start `nexo supervise`, \
         or open /panel in nexo."
    );
    0
}
