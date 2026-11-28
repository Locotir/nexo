//! Loading the configuration of a folder and asking whether to trust it.

use super::*;

/// A file that cannot be parsed already names itself and the line; this adds where to look next, on
/// the same line, so a person who sees only this message still has a command to run.
pub(crate) fn point_to_the_files(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<nexo_core::config::ConfigError>() {
        Some(nexo_core::config::ConfigError::Parse(..)) => {
            anyhow::anyhow!("{e:#}; run `nexo doctor` or `nexo config path` to see the files")
        }
        _ => e,
    }
}

/// What `nexo doctor` prints under the file it could not load: the way out that fits that file. The
/// user file can be moved aside and started over with `config init`; the project file needs
/// `--project` for that; an administrator policy is not the person's to move or bypass; a file that
/// exists but cannot be read is a matter of permissions, not of its content.
pub(super) fn way_out(e: &anyhow::Error) -> String {
    use nexo_core::config::ConfigError;
    match e.downcast_ref::<ConfigError>() {
        Some(ConfigError::Managed(_)) => {
            "this is the administrator policy: ask whoever manages it for a file that nexo can read".into()
        }
        Some(ConfigError::Io(..)) => "check that the file named above exists and that you may read it".into(),
        Some(ConfigError::Parse(file, _)) if *file == nexo_core::paths::user_config_file() => {
            "fix the file named above, or move it aside and run `nexo config init`".into()
        }
        Some(ConfigError::Parse(..)) => {
            "fix the file named above, or move it aside and run `nexo config init --project`".into()
        }
        None => "fix the problem named above".into(),
    }
}

/// The configuration of `cwd` with the presets and the providers the person asked for.
pub(super) fn load_config(args: &Args, cwd: &std::path::Path) -> anyhow::Result<nexo_core::Config> {
    let mut config = load_config_for(args.provider.as_deref(), cwd)?;
    apply_audit_flag(&mut config, args.audit);
    Ok(config)
}

pub(crate) fn apply_audit_flag(config: &mut nexo_core::Config, audit: bool) {
    if audit {
        config.audit.enabled = true;
    }
}

/// The configuration of `cwd` with the presets and `provider` (the one asked for, if any) ready to use.
pub(crate) fn load_config_for(provider: Option<&str>, cwd: &std::path::Path) -> anyhow::Result<nexo_core::Config> {
    let mut config = nexo_core::Config::load(cwd)?;
    setup::apply_presets(&mut config);
    // a provider asked for by a name nexo knows works without a block in the file
    for p in [provider.map(String::from), config.provider.clone()].into_iter().flatten() {
        crate::account::adopt(&mut config, &p);
    }
    config.name_auth_entries();
    // the presets and the providers just adopted come under the managed policy too
    config.enforce_managed();
    Ok(config)
}

/// Before the first prompt of the interface: a folder not trusted yet that holds project hooks,
/// servers or commands is asked about, and a yes is recorded as `nexo config trust` records it.
/// True when it was: the configuration is then read again, with the project's own files in it.
/// A no, a headless run (`-p`), a subcommand and a start without a terminal ask nothing; the
/// notice the loading prints stays. The managed policy leaves out what it forbids from the question.
pub(super) async fn trust_asked(args: &Args, cwd: &std::path::Path, config: &nexo_core::Config) -> bool {
    use std::io::IsTerminal;
    let tty = (std::io::stdin().is_terminal(), std::io::stdout().is_terminal());
    if !nexo_core::trust::asks(args.prompt.is_some(), args.cmd.is_some(), tty.0, tty.1) {
        return false;
    }
    let Some(gate) = nexo_core::trust::gate_here(cwd, config.managed.as_deref()) else { return false };
    let ask =
        nexo_tui::TrustAsk { path: gate.root.display().to_string(), enables: gate.enables(), warning: gate.warning() };
    let opts = nexo_tui::TuiOptions {
        cwd: cwd.to_path_buf(),
        theme: args.theme.clone().unwrap_or_else(|| config.ui.theme.clone()),
        mouse: config.ui.mouse && !args.no_mouse,
        animations: config.ui.animations,
        detail: config.ui.detail,
        ..Default::default()
    };
    match nexo_tui::ask_trust(ask, opts).await {
        Ok(true) => match gate.record(&nexo_core::paths::trusted_projects_file()) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("nexo: cannot record the trust of {}: {e}", gate.root.display());
                false
            }
        },
        Ok(false) => false,
        Err(e) => {
            eprintln!("nexo: cannot ask about trusting {}: {e:#}", gate.root.display());
            false
        }
    }
}
