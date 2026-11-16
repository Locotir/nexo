//! The alert hook: the person's own command, run without a shell, with `{agent}`, `{state}` and `{why}`
//! replaced inside its arguments. What goes in is redacted and cut; the command is never waited for.

use std::process::{Command, Stdio};

/// The argument list with the markers filled in. Empty when there is no command.
pub fn expand(argv: &[String], agent: &str, state: &str, why: &str) -> Vec<String> {
    let clean = |s: &str| -> String {
        let r = nexo_core::redact::redact(s);
        r.chars().filter(|c| !c.is_control() || *c == ' ').take(200).collect()
    };
    let (a, s, w) = (clean(agent), clean(state), clean(why));
    argv.iter().map(|x| x.replace("{agent}", &a).replace("{state}", &s).replace("{why}", &w)).collect()
}

/// Run the command. Ok(false) when none is set; the child is reaped in the background.
pub fn run_alert(argv: &[String], agent: &str, state: &str, why: &str) -> std::io::Result<bool> {
    let args = expand(argv, agent, state, why);
    let Some((prog, rest)) = args.split_first() else { return Ok(false) };
    if prog.is_empty() {
        return Ok(false);
    }
    let mut child =
        Command::new(prog).args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_filled_inside_each_argument_and_nothing_goes_through_a_shell() {
        let argv = vec!["notify".to_string(), "nexo: {agent}".into(), "{state}: {why}".into()];
        let out = expand(&argv, "build; rm -rf /", "stalled", "no progress\nfor 40 min");
        assert_eq!(out[0], "notify");
        assert_eq!(out[1], "nexo: build; rm -rf /", "kept as one literal argument");
        assert_eq!(out[2], "stalled: no progressfor 40 min");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn secrets_never_reach_the_command() {
        let secret = format!("sk-{}", "Zz9Yy8Xx".repeat(5));
        let out = expand(&["x".into(), "{why}".into()], "a", "s", &format!("key {secret}"));
        assert!(!out[1].contains(&secret));
    }

    #[test]
    fn no_command_means_no_alert() {
        assert!(!run_alert(&[], "a", "s", "w").unwrap());
        assert!(run_alert(&["/definitely/not/here".into()], "a", "s", "w").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_command_runs_with_its_arguments() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out.txt");
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".into(),
            "printf '%s' \"$1\" > \"$2\"".into(),
            "x".into(),
            "{agent}/{state}".into(),
            out.display().to_string(),
        ];
        assert!(run_alert(&argv, "qa", "looping", "w").unwrap());
        for _ in 0..100 {
            if out.exists() && std::fs::metadata(&out).map(|m| m.len() > 0).unwrap_or(false) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "qa/looping");
    }
}
