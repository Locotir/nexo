//! Helpers of the slash commands: signing out and where a command of one's own comes from.

use super::*;

/// Removes from the auth store `file` what is kept for `name` (else `current`) under its `auth` entry;
/// what to tell the person. A key from an environment variable or the configuration is not nexo's to remove.
pub(super) fn sign_out(
    providers: &[(String, nexo_core::ProviderConfig)],
    current: &str,
    name: Option<&str>,
    file: &std::path::Path,
) -> (Level, String) {
    let name = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or(current);
    let entry = providers.iter().find(|(n, _)| n == name).and_then(|(_, p)| p.auth.as_deref()).unwrap_or(name);
    match nexo_core::auth::remove_in(file, entry) {
        Ok(true) => (Level::Info, format!("{name}: signed out, the stored key is removed; /model signs in again")),
        Ok(false) => (
            Level::Warn,
            format!("{name}: nothing stored to remove (a key from an environment variable or the configuration stays)"),
        ),
        Err(e) => (Level::Error, format!("{name}: cannot change {}: {e}", file.display())),
    }
}

/// Where a command of one's own comes from, as the palette shows it: `~/.claude/commands`, `project .nexo/commands`,
/// `skill`, `plugin <name>`; `custom` when it is not known.
pub(super) fn custom_origin(origin: &str) -> String {
    match origin.split_once(' ') {
        Some(("user", dir)) => format!("~/{dir}"),
        _ if origin.is_empty() => "custom".into(),
        _ => origin.into(),
    }
}

#[cfg(test)]
mod custom_origin_tests {
    #[test]
    fn a_command_of_ones_own_names_the_folder_it_comes_from() {
        assert_eq!(super::custom_origin("user .claude/commands"), "~/.claude/commands");
        assert_eq!(super::custom_origin("project .nexo/commands"), "project .nexo/commands");
        assert_eq!(super::custom_origin("plugin tidy"), "plugin tidy");
        assert_eq!(super::custom_origin("skill"), "skill");
        assert_eq!(super::custom_origin(""), "custom");
    }
}
