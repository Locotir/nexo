//! The terminal's own side of the interface: the thread that reads its events, the hangup
//! signals, the bell and desktop notification, and the external editor.

use std::time::Duration;

use crate::trace;

/// A paste that came as keys, whole: into the draft as one paste.
/// Terminal events read on a thread of their own, with a level-triggered `poll`:
/// crossterm's async stream stalls on unix after the first KiB of a big paste
/// until the next key. Paused while an editor has the terminal.
pub(crate) struct Keys {
    rx: tokio::sync::mpsc::UnboundedReceiver<std::io::Result<crossterm::event::Event>>,
    paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Keys {
    pub(crate) fn spawn() -> Self {
        use std::sync::atomic::Ordering;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let paused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = paused.clone();
        std::thread::spawn(move || loop {
            if flag.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            let read = match crossterm::event::poll(Duration::from_millis(200)) {
                Ok(true) if !flag.load(Ordering::Acquire) => crossterm::event::read(),
                Ok(_) => continue,
                Err(e) => Err(e),
            };
            if let Ok(e) = &read {
                trace::event(e);
            }
            let failed = read.is_err();
            if tx.send(read).is_err() || failed {
                break;
            }
        });
        Keys { rx, paused }
    }

    pub(crate) async fn next(&mut self) -> Option<std::io::Result<crossterm::event::Event>> {
        self.rx.recv().await
    }

    pub(crate) fn pause(&self, on: bool) {
        self.paused.store(on, std::sync::atomic::Ordering::Release);
    }
}

/// SIGTERM and SIGHUP (`kill`, the window closed): the run loop ends the normal way, so the
/// terminal is given back (raw mode, alternate screen, mouse capture, bracketed paste).
/// On Windows the closed console, logoff and shutdown take their place. Elsewhere it never fires.
pub(crate) struct Hangup {
    #[cfg(unix)]
    signals: Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>,
    #[cfg(windows)]
    signals: Option<(
        tokio::signal::windows::CtrlClose,
        tokio::signal::windows::CtrlShutdown,
        tokio::signal::windows::CtrlLogoff,
    )>,
}

impl Hangup {
    pub(crate) fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Hangup { signals: signal(SignalKind::terminate()).ok().zip(signal(SignalKind::hangup()).ok()) }
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows::{ctrl_close, ctrl_logoff, ctrl_shutdown};
            Hangup {
                signals: ctrl_close()
                    .ok()
                    .zip(ctrl_shutdown().ok())
                    .zip(ctrl_logoff().ok())
                    .map(|((c, s), l)| (c, s, l)),
            }
        }
        #[cfg(not(any(unix, windows)))]
        Hangup {}
    }

    pub(crate) async fn wait(&mut self) {
        #[cfg(unix)]
        if let Some((term, hup)) = self.signals.as_mut() {
            let got = tokio::select! { t = term.recv() => t, h = hup.recv() => h };
            if got.is_some() {
                return;
            }
            // the signal driver is gone: nothing more will come
            self.signals = None;
        }
        #[cfg(windows)]
        if let Some((close, shutdown, logoff)) = self.signals.as_mut() {
            // Windows ends the process a few seconds after the close event, so the caller must leave at once
            let got = tokio::select! { c = close.recv() => c, s = shutdown.recv() => s, l = logoff.recv() => l };
            if got.is_some() {
                return;
            }
            // the signal driver is gone: nothing more will come
            self.signals = None;
        }
        std::future::pending::<()>().await
    }
}

/// What a long turn's end writes to the terminal: the bell, then the desktop notification in
/// the two forms terminals take (OSC 9: iTerm2, WezTerm, kitty, Windows Terminal, foot;
/// OSC 777: GNOME Terminal, Konsole and other VTE ones). A terminal that knows neither
/// ignores them. Control characters in the text are dropped so it cannot end the sequence.
pub fn notification(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    format!("\x07\x1b]9;{clean}\x07\x1b]777;notify;nexo;{clean}\x07")
}

/// On a Mac, the program that shows the desktop notification for terminals that read neither OSC 9 nor OSC 777
/// (Terminal.app and VS Code's terminal), or `None` where the sequences of [`notification`] do. The text is the last
/// argument and never part of the script, so it cannot be taken for code.
pub fn notify_command(term_program: Option<&str>, text: &str) -> Option<(&'static str, Vec<String>)> {
    if !matches!(term_program, Some("Apple_Terminal" | "vscode")) {
        return None;
    }
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    let script = ["on run argv", "display notification (item 1 of argv) with title \"nexo\"", "end run"];
    let mut args: Vec<String> = script.iter().flat_map(|l| ["-e".to_string(), l.to_string()]).collect();
    args.push(clean);
    Some(("osascript", args))
}

/// What to say once the editor closed: a file left as it was is not "saved".
pub(crate) fn edit_notice(path: &str, before: Option<Vec<u8>>, after: Option<Vec<u8>>) -> String {
    if before == after {
        format!("{path} not changed")
    } else {
        format!("{path} saved; it applies from the next request")
    }
}

/// Opens `path` in `$VISUAL`, else `$EDITOR`, else Notepad on Windows and `nano`, else `vi`,
/// elsewhere, and waits.
pub(crate) fn edit_file(path: &std::path::Path) -> anyhow::Result<()> {
    let mut last = None;
    for words in nexo_core::shell::editors() {
        let Some((bin, args)) = words.split_first() else { continue };
        // the screen is the terminal's own while the editor runs: say where nexo went and how to return
        let name =
            std::path::Path::new(bin).file_name().map_or_else(|| bin.clone(), |n| n.to_string_lossy().into_owned());
        println!("nexo: opening {} in {name}; save and quit {name} to come back", path.display());
        let status = std::process::Command::new(nexo_core::shell::program(bin)).args(args).arg(path).status();
        match status {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => anyhow::bail!("{bin} exited with {s}"),
            Err(e) => last = Some(format!("{bin}: {e}")),
        }
    }
    anyhow::bail!(last.unwrap_or_else(|| "no editor found: set $VISUAL or $EDITOR".into()))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn a_hangup_or_a_terminate_signal_ends_the_wait() {
        // the handlers are in place before the signal is sent, so the test process lives on
        for sig in ["-HUP", "-TERM"] {
            let mut h = super::Hangup::new();
            let pid = std::process::id().to_string();
            // the shell's own `kill`: other tests change PATH while this one runs
            let kill = format!("kill {sig} {pid}");
            assert!(std::process::Command::new("/bin/sh").args(["-c", &kill]).status().unwrap().success());
            let ended = tokio::time::timeout(std::time::Duration::from_secs(10), h.wait()).await;
            assert!(ended.is_ok(), "{sig} did not end the wait");
        }
    }

    #[test]
    fn an_editor_closed_without_a_change_does_not_say_saved() {
        let (a, b) = (Some(b"x".to_vec()), Some(b"y".to_vec()));
        assert_eq!(super::edit_notice("f", a.clone(), a.clone()), "f not changed");
        assert_eq!(super::edit_notice("f", None, None), "f not changed");
        assert!(super::edit_notice("f", a, b.clone()).contains("saved"));
        assert!(super::edit_notice("f", None, b).contains("saved"));
    }

    #[test]
    fn the_notification_is_the_bell_and_both_desktop_forms() {
        assert_eq!(
            super::notification("nexo: done after 20s"),
            "\x07\x1b]9;nexo: done after 20s\x07\x1b]777;notify;nexo;nexo: done after 20s\x07"
        );
        assert!(!super::notification("a\x07b\x1bc").contains("a\x07"), "the text cannot close the sequence");
    }

    #[test]
    fn terminal_app_gets_a_system_notification_and_iterm_keeps_the_sequences() {
        let (prog, args) = super::notify_command(Some("Apple_Terminal"), "done \"x\"; quit\n").unwrap();
        assert_eq!(prog, "osascript");
        assert_eq!(args.last().unwrap(), "done \"x\"; quit", "the text is one argument, control characters dropped");
        assert!(args[..args.len() - 1].iter().all(|a| !a.contains("quit")));
        assert!(super::notify_command(Some("iTerm.app"), "x").is_none());
        assert!(super::notify_command(None, "x").is_none());
    }
}
