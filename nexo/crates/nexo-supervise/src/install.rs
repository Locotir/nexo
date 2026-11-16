//! The hook entries a person adds to the settings of an external session tool so its stop, permission and
//! notification events reach the supervisor. This only builds the text; nothing here writes a file.

use serde_json::json;

/// The command for one hook kind, with the program path quoted when it holds a space.
pub fn hook_command(exe: &str, kind: &str) -> String {
    if exe.contains(' ') {
        format!("\"{exe}\" supervise hook {kind}")
    } else {
        format!("{exe} supervise hook {kind}")
    }
}

/// The JSON object to merge into the `hooks` section of the settings file.
pub fn snippet(exe: &str) -> String {
    let entry = |kind: &str, secs: u32| json!([{ "hooks": [{ "type": "command", "command": hook_command(exe, kind), "timeout": secs }] }]);
    let doc = json!({
        "hooks": {
            "Stop": entry("stop", 5),
            "PermissionRequest": entry("permission", 5),
            "Notification": entry("notify", 5),
        }
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snippet_names_the_three_hooks_and_the_given_program() {
        let s = snippet("/opt/nexo/bin/nexo");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "/opt/nexo/bin/nexo supervise hook stop");
        assert_eq!(
            v["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "/opt/nexo/bin/nexo supervise hook permission"
        );
        assert_eq!(v["hooks"]["Notification"][0]["hooks"][0]["command"], "/opt/nexo/bin/nexo supervise hook notify");
    }

    #[test]
    fn a_path_with_a_space_is_quoted() {
        assert_eq!(hook_command("/a b/nexo", "stop"), "\"/a b/nexo\" supervise hook stop");
    }
}
