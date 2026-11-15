use super::FLOWS;

/// A way to sign in, for a list: the flow to run, a label and a line on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub flow: String,
    pub label: String,
    pub hint: String,
}

/// The ways to sign in to `flow`'s subscription: the Codex CLI's login first when one is on this
/// machine, then the browser, then the device code. Other subscriptions have one way.
pub fn choices(flow: &str) -> Vec<Choice> {
    choices_in(flow, &crate::codex::cli_file())
}

pub fn choices_in(flow: &str, codex_file: &std::path::Path) -> Vec<Choice> {
    let pick = |f: &str, label: &str, hint: &str| Choice { flow: f.into(), label: label.into(), hint: hint.into() };
    if flow != "codex" {
        let what = FLOWS.iter().find(|(f, _)| *f == flow).map(|(_, w)| *w).unwrap_or("");
        return vec![pick(flow, "Sign in", what)];
    }
    let mut out = Vec::new();
    if crate::codex::cli_login_in(codex_file) {
        out.push(pick(
            "codex-cli",
            "Use the Codex CLI login",
            "found on this machine; nothing to type, the tokens are not shown",
        ));
    }
    out.push(pick(
        "codex-browser",
        "Sign in with ChatGPT in the browser",
        "opens a page; the answer comes back to localhost:1455",
    ));
    out.push(pick(
        "codex-device",
        "Sign in with a device code",
        "for a machine without a browser: type a code on another device",
    ));
    out
}

/// Opens `url` in the default browser; false when nothing could be started (the person opens it).
pub fn open_url(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}
