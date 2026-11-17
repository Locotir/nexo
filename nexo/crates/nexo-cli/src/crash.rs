//! What a panic shows: one plain line, the details in a file, the terminal given back when the interface itself
//! died, instead of the raw Rust message.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a panic already printed its line, so that the exit of the main thread does not print a second one.
static PRINTED: AtomicBool = AtomicBool::new(false);

/// The line a person reads: where it happened and where the details are.
fn crash_line(at: Option<(&str, u32)>, details: Option<&Path>) -> String {
    let mut line = match at {
        Some((file, n)) => format!("nexo hit an internal error at {file}:{n}."),
        None => "nexo hit an internal error.".to_owned(),
    };
    if let Some(path) = details {
        line.push_str(&format!(" Details: {}", path.display()));
    }
    line
}

/// The file's text: what a bug report needs.
fn crash_report(thread: &str, at: Option<(&str, u32)>, message: &str, backtrace: &str) -> String {
    let at = at.map(|(f, n)| format!("{f}:{n}")).unwrap_or_else(|| "unknown".into());
    format!(
        "nexo {}\nthread: {thread}\nat: {at}\nmessage: {message}\n\nbacktrace:\n{backtrace}\n",
        env!("NEXO_VERSION")
    )
}

/// Writes the report under the data dir (resolved here, after `adopt_root_home` ran) and returns where it went.
fn write_report(text: &str) -> Option<PathBuf> {
    let dir = nexo_core::paths::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = time::OffsetDateTime::now_utc()
        .format(time::macros::format_description!("[year][month][day]-[hour][minute][second]"))
        .unwrap_or_else(|_| "unknown-time".into());
    let path = dir.join(format!("crash-{stamp}.log"));
    std::fs::write(&path, text).ok()?;
    Some(path)
}

fn payload_text(info: &std::panic::PanicHookInfo<'_>) -> String {
    let p = info.payload();
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

fn hook(info: &std::panic::PanicHookInfo<'_>) {
    let message = payload_text(info);
    // A closed stdout (`nexo providers | head`) ends the process quietly instead of panicking. SIGPIPE stays
    // ignored: a hook, MCP server or tool that exits before reading its stdin must not kill nexo, so only the print
    // that hit the closed stdout ends it.
    if message.starts_with("failed printing to stdout") && message.contains("Broken pipe") {
        std::process::exit(141);
    }
    let at = info.location().map(|l| (l.file(), l.line()));
    let thread = std::thread::current().name().unwrap_or("unnamed").to_owned();
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    let path = write_report(&crash_report(&thread, at, &message, &trace));
    let interface_died = nexo_tui::terminal::on_tui_thread();
    if interface_died {
        nexo_tui::terminal::restore();
    } else if nexo_tui::terminal::holds_terminal() {
        // A tool or provider task panicked: tokio catches it and the interface keeps running, so text printed now
        // would land in the middle of the screen. The report file has it.
        return;
    }
    eprintln!("{}", crash_line(at, path.as_deref()));
    if let Some(id) = nexo_tui::terminal::noted_session() {
        eprintln!("The session is kept: nexo --resume {id}");
    }
    PRINTED.store(true, Ordering::Relaxed);
}

pub fn install() {
    std::panic::set_hook(Box::new(hook));
}

/// The main thread ended in a panic: the hook said so already, unless it could not.
pub fn after_main_panic() -> i32 {
    if !PRINTED.load(Ordering::Relaxed) {
        eprintln!("{}", crash_line(None, None));
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crash_line_names_the_place_and_the_report_file() {
        let line = crash_line(Some(("src/tool.rs", 42)), Some(Path::new("/home/me/.nexo/crash-1.log")));
        assert_eq!(line, "nexo hit an internal error at src/tool.rs:42. Details: /home/me/.nexo/crash-1.log");
    }

    #[test]
    fn the_crash_line_does_not_invent_a_place_or_a_file() {
        assert_eq!(crash_line(None, None), "nexo hit an internal error.");
    }

    #[test]
    fn the_crash_report_carries_the_message_the_thread_and_the_backtrace() {
        let text = crash_report("tokio-runtime-worker", Some(("a.rs", 7)), "index out of bounds", "0: frame");
        for part in ["thread: tokio-runtime-worker", "at: a.rs:7", "message: index out of bounds", "0: frame"] {
            assert!(text.contains(part), "missing {part:?} in {text}");
        }
    }
}
