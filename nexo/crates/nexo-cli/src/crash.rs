//! What a panic shows: one plain line, the details in a file, the terminal given back when the interface itself
//! died, instead of the raw Rust message.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// Lines of panics that happened while the interface held the screen, kept for when the terminal is given back:
/// printing them then would land in the middle of the live screen.
static HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// How many held lines are printed; the report files have the rest.
const HELD_SHOWN: usize = 3;

/// Writes to stderr and ignores a failure: a closed stderr (`nexo ... 2>&1 | head`) makes `eprintln!` panic, and a
/// panic inside the panic hook aborts the process instead of letting it exit.
fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Writes the report under the data dir (resolved here, after `adopt_root_home` ran) and returns where it went.
fn write_report(text: &str) -> Option<PathBuf> {
    let dir = nexo_core::paths::data_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = time::OffsetDateTime::now_utc()
        .format(time::macros::format_description!("[year][month][day]-[hour][minute][second]"))
        .unwrap_or_else(|_| "unknown-time".into());
    write_report_in(&dir, &stamp, text)
}

/// Creates `crash-<stamp>.log` in `dir`, or the first free `crash-<stamp>-<n>.log` when that name is taken: several
/// panics in one second must each keep their report. The file is private (the message can carry a URL or prompt
/// text) and never replaces an existing one.
fn write_report_in(dir: &Path, stamp: &str, text: &str) -> Option<PathBuf> {
    for n in 1..=50u32 {
        let name = if n == 1 { format!("crash-{stamp}.log") } else { format!("crash-{stamp}-{n}.log") };
        let path = dir.join(name);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        match o.open(&path) {
            Ok(mut f) => return f.write_all(text.as_bytes()).ok().map(|()| path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Keeps a crash line for `print_held`.
fn hold(line: String) {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).push(line);
}

/// The held lines as they are printed: the first few, and how many more the data dir has.
fn held_text(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().take(HELD_SHOWN).cloned().collect();
    if lines.len() > HELD_SHOWN {
        out.push(format!("{} more crash reports are in the same folder.", lines.len() - HELD_SHOWN));
    }
    out
}

/// Prints what panics said while the interface held the screen. Called when the process ends, after the terminal was
/// released, so that a quit caused by a dead agent task does not look like a normal one.
pub fn print_held() {
    let lines = std::mem::take(&mut *HELD.lock().unwrap_or_else(|e| e.into_inner()));
    for line in held_text(&lines) {
        say(&line);
    }
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
        // would land in the middle of the screen. The report file has it, and the line is printed when nexo ends.
        hold(crash_line(at, path.as_deref()));
        return;
    }
    say(&crash_line(at, path.as_deref()));
    if let Some(id) = nexo_tui::terminal::noted_session() {
        say(&format!("The session is kept: nexo --resume {id}"));
    }
    PRINTED.store(true, Ordering::Relaxed);
}

pub fn install() {
    std::panic::set_hook(Box::new(hook));
}

/// The main thread ended in a panic: the hook said so already, unless it could not.
pub fn after_main_panic() -> i32 {
    if !PRINTED.load(Ordering::Relaxed) {
        say(&crash_line(None, None));
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

    #[test]
    fn two_panics_in_one_second_keep_both_reports() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_report_in(dir.path(), "20261117-010000", "first").unwrap();
        let second = write_report_in(dir.path(), "20261117-010000", "second").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
    }

    #[cfg(unix)]
    #[test]
    fn a_crash_report_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = write_report_in(dir.path(), "20261117-010000", "message with a url").unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn panics_under_the_live_screen_are_printed_once_it_is_given_back() {
        hold("nexo hit an internal error at a.rs:1. Details: /d/crash-1.log".into());
        let held = HELD.lock().unwrap().clone();
        assert_eq!(held_text(&held), ["nexo hit an internal error at a.rs:1. Details: /d/crash-1.log"]);
        HELD.lock().unwrap().clear();
        let many: Vec<String> = (1..=5).map(|n| format!("line {n}")).collect();
        let shown = held_text(&many);
        assert_eq!(shown.len(), HELD_SHOWN + 1);
        assert_eq!(shown.last().unwrap(), "2 more crash reports are in the same folder.");
    }
}
