use super::*;

/// Where the output goes and how it looks. On a terminal each step is a spinner line that a background
/// ticker redraws about 12 times a second (and only while a step runs) and that turns into a `✓` line when
/// the step ends; anywhere else (a pipe, `NO_COLOR`, `TERM=dumb`) it is plain lines with no control codes.
pub(super) struct Ui {
    fancy: bool,
    shared: std::sync::Arc<Shared>,
    ticker: Option<std::thread::JoinHandle<()>>,
}

pub(super) struct Shared {
    live: std::sync::Mutex<Option<Live>>,
    stop: std::sync::atomic::AtomicBool,
}

/// The step on screen: its label, when it began, an optional detail (the crate cargo is on) and download progress.
pub(super) struct Live {
    label: String,
    since: Instant,
    detail: String,
    bytes: Option<(u64, Option<u64>)>,
}

/// One frame of the ticker: about 12 per second, the one rate nexo moves at.
pub(super) const FRAME: Duration = Duration::from_millis(83);

impl Ui {
    /// Fancy when stdout and stderr are terminals that take escape codes, `NO_COLOR` is not set and `TERM` is not `dumb`.
    pub(super) fn detect() -> Self {
        use std::io::IsTerminal;
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let tty = std::io::stderr().is_terminal()
            && std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && !dumb;
        #[cfg(windows)]
        let tty = tty && crossterm::ansi_support::supports_ansi();
        Self::new(tty)
    }

    pub(super) fn new(fancy: bool) -> Self {
        let shared = std::sync::Arc::new(Shared { live: std::sync::Mutex::new(None), stop: false.into() });
        let ticker = fancy.then(|| {
            eprint!("{HIDE_CURSOR}");
            let shared = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || {
                while !shared.stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(FRAME);
                    shared.draw();
                }
            })
        });
        Ui { fancy, shared, ticker }
    }

    /// Starts a step. Plain mode says nothing unless `announce` (a step that takes minutes).
    pub(super) fn begin(&self, label: &str, announce: bool) {
        if self.fancy {
            let live = Live { label: label.into(), since: Instant::now(), detail: String::new(), bytes: None };
            *self.shared.lock() = Some(live);
            self.shared.draw();
        } else if announce {
            println!("{}", styled(false, Kind::Step, label));
        }
    }

    /// Ends the step: the spinner line becomes `msg` as a finished line of `kind`.
    pub(super) fn end(&self, kind: Kind, msg: &str) {
        self.note(kind, msg);
        *self.shared.lock() = None;
    }

    /// Prints a line above the running step.
    pub(super) fn note(&self, kind: Kind, msg: &str) {
        if self.fancy {
            let live = self.shared.lock();
            let text = styled(true, kind, msg);
            let mut e = std::io::stderr();
            use std::io::Write;
            let _ = write!(e, "\r{text}{CLEAR_REST}\n").and_then(|()| e.flush());
            drop(live);
            self.shared.draw();
        } else if matches!(kind, Kind::Fail) {
            eprintln!("{}", styled(false, kind, msg));
        } else {
            println!("{}", styled(false, kind, msg));
        }
    }

    /// Wipes the live line without a result (the caller is about to fail or hand over the terminal).
    pub(super) fn abort(&self) {
        *self.shared.lock() = None;
        if self.fancy {
            eprint!("\r{CLEAR_LINE}");
        }
    }

    pub(super) fn progress(&self, done: u64, total: Option<u64>) {
        if let Some(l) = self.shared.lock().as_mut() {
            l.bytes = Some((done, total));
        }
    }

    pub(super) fn detail(&self, text: &str) {
        if let Some(l) = self.shared.lock().as_mut() {
            if l.detail != text {
                l.detail = text.into();
            }
        }
    }

    /// On Ctrl+C: wipes the live line, brings the cursor back, removes `stage` and leaves with 130.
    pub(super) fn on_interrupt(&self, stage: &Path) {
        if !self.fancy {
            return;
        }
        let stage = stage.to_path_buf();
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            if rt.block_on(tokio::signal::ctrl_c()).is_ok() {
                eprint!("\r{CLEAR_LINE}{SHOW_CURSOR}");
                eprintln!("{}", styled(true, Kind::Warn, "update cancelled"));
                let _ = std::fs::remove_dir_all(stage);
                std::process::exit(130);
            }
        });
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.shared.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
            eprint!("\r{CLEAR_LINE}{SHOW_CURSOR}");
        }
    }
}

impl Shared {
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Option<Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Redraws the live line if a step is running (one write, so a frame never shows half drawn).
    pub(super) fn draw(&self) {
        let live = self.lock();
        let Some(l) = live.as_ref() else { return };
        let cols = crossterm::terminal::size().map_or(80, |(c, _)| c as usize);
        let frame = (l.since.elapsed().as_millis() / FRAME.as_millis()) as usize;
        let text = live_line(frame, &l.label, &l.detail, l.since.elapsed().as_secs_f64(), l.bytes, cols);
        use std::io::Write;
        let mut e = std::io::stderr();
        let _ = write!(e, "\r{CYAN}{text}{RESET}{CLEAR_REST}").and_then(|()| e.flush());
    }
}

pub(super) const CYAN: &str = "\x1b[36m";
pub(super) const GREEN: &str = "\x1b[32m";
pub(super) const YELLOW: &str = "\x1b[33m";
pub(super) const RED: &str = "\x1b[31m";
pub(super) const DIM: &str = "\x1b[2m";
pub(super) const RESET: &str = "\x1b[0m";
pub(super) const CLEAR_REST: &str = "\x1b[K";
pub(super) const CLEAR_LINE: &str = "\x1b[2K";
pub(super) const HIDE_CURSOR: &str = "\x1b[?25l";
pub(super) const SHOW_CURSOR: &str = "\x1b[?25h";

/// The kinds of line `nexo update` prints.
#[derive(Clone, Copy)]
pub(super) enum Kind {
    /// A step that begins (plain mode only).
    Step,
    /// A step that ended well.
    Ok,
    /// Something to know, not an error.
    Warn,
    /// A side remark.
    Dim,
    /// A step that failed.
    Fail,
    /// An update is waiting (`--check`): a warning with a symbol, a bare line in plain mode.
    Notice,
    /// The one closing line: where nexo is now.
    Final,
}

/// A line as shown: with colors and symbols when `fancy`, else plain `nexo: <message>` with no escape codes
/// (the closing line has no prefix).
pub(super) fn styled(fancy: bool, kind: Kind, msg: &str) -> String {
    if !fancy {
        return if matches!(kind, Kind::Final | Kind::Notice) { msg.to_string() } else { format!("nexo: {msg}") };
    }
    match kind {
        Kind::Step => format!("nexo {CYAN}▸{RESET} {msg}"),
        Kind::Ok => format!("{GREEN}✓{RESET} {msg}"),
        Kind::Final => format!("{GREEN}✓{RESET} {msg}"),
        Kind::Warn | Kind::Notice => format!("{YELLOW}⚠{RESET} {msg}"),
        Kind::Fail => format!("{RED}✗{RESET} {msg}"),
        Kind::Dim => format!("{DIM}  {msg}{RESET}"),
    }
}

/// The last line: where nexo was and where it is now (each left out when unknown).
pub(super) fn update_text(new: Option<&str>, old: &str) -> String {
    let old = if old == "unknown" { String::new() } else { format!(" ({})", short(old)) };
    let new = new.map_or("the latest build".to_string(), |n| short(n).to_string());
    format!("nexo {}{old} → {new}. Restart nexo.", env!("CARGO_PKG_VERSION"))
}

pub(super) fn secs_text(d: Duration) -> String {
    format!("{} s", d.as_secs_f64().round() as u64)
}

pub(super) fn megabytes(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1e6)
}

/// The live line: a spinner frame, the step, then the elapsed seconds (fixed width, so the line never shifts);
/// for a download a bar, percent, done of total and speed, which drops its bar and then its tail as the
/// terminal narrows; without a known length, bytes and speed. Never longer than `cols - 1`.
pub(super) fn live_line(
    frame: usize,
    label: &str,
    detail: &str,
    secs: f64,
    bytes: Option<(u64, Option<u64>)>,
    cols: usize,
) -> String {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let spin = FRAMES[frame % FRAMES.len()];
    let tail = match bytes {
        None => {
            let d = if detail.is_empty() { String::new() } else { format!("  {detail}") };
            format!("{:>3} s{d}", secs as u64)
        }
        Some((done, total)) => {
            let speed = format!("{:>8}/s", megabytes((done as f64 / secs.max(0.001)) as u64));
            match total.filter(|t| *t > 0) {
                Some(t) => {
                    let frac = (done as f64 / t as f64).clamp(0.0, 1.0);
                    let w = megabytes(t).len();
                    let stats =
                        format!("{:>3}%  {:>w$} / {}  {speed}", (frac * 100.0) as u32, megabytes(done), megabytes(t));
                    let room = cols.saturating_sub(1 + 2 + label.chars().count() + 2 + stats.chars().count() + 2);
                    let width = room.min(24);
                    if width >= 8 {
                        let full = (frac * width as f64) as usize;
                        format!("{}{}  {stats}", "█".repeat(full), "░".repeat(width - full))
                    } else {
                        stats
                    }
                }
                None => format!("{:>8}  {speed}", megabytes(done)),
            }
        }
    };
    let line = format!("{spin} {label}  {tail}");
    line.chars().take(cols.saturating_sub(1).max(1)).collect()
}

/// A reqwest error without the long address: `... for url (https://host/a/b/FILE)` becomes `... (FILE)`, and
/// `HTTP status server error (504 Gateway Timeout)` becomes `HTTP 504 Gateway Timeout`.
pub(super) fn short_error(e: &str) -> String {
    let Some((what, rest)) = e.split_once(" for url (") else { return e.to_string() };
    let url = rest.strip_suffix(')').unwrap_or(rest);
    let file = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or(url);
    let what = match (what.starts_with("HTTP status "), what.split_once('(')) {
        (true, Some((_, code))) => format!("HTTP {}", code.trim_end_matches(')')),
        _ => what.to_string(),
    };
    format!("{what} ({file})")
}
