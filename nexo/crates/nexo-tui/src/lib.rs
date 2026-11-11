//! The terminal interface of nexo: a full-screen pager over the agent's
//! events, an editor, a palette and modals. It depends on `nexo-core` only
//! and talks to the agent through [`nexo_core::agent::AgentHandle`].

/// Tests that change the environment (PATH and the like) and tests that start programs
/// found on PATH take this lock, so one never runs while the other has PATH swapped.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub mod agents_view;
pub mod anim;
pub mod app;
pub mod app_fx;
pub mod ascii;
pub mod clipboard;
pub mod commands;
pub mod complete;
pub mod config_edit;
pub mod curated;
pub mod custom;
pub mod dialog_size;
pub mod diffkey;
pub mod diffview;
pub mod dropped;
pub mod event;
pub mod frame;
pub mod frame_stats;
pub mod fuzzy;
pub mod fx_chrome;
pub mod fx_diff;
pub mod fx_effort;
pub mod fx_galaxy;
pub mod fx_goal;
pub mod fx_stars;
pub mod fx_stream;
pub mod goal_view;
pub mod inline;
pub mod input;
pub mod insights;
pub mod jobs_view;
pub mod keymap;
pub mod links;
pub mod mcp_view;
pub mod modal;
pub mod motion;
pub mod overlay;
pub mod palette;
pub mod palette_key;
pub mod panel_fx;
pub mod regions;
pub mod render;
pub mod replay;
pub mod scrollbar;
pub mod select;
pub mod sent_view;
pub mod status_cmd;
pub mod terminal;
pub mod theme;
pub mod toast;
pub mod todo_strip;
pub mod todo_view;
pub mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nexo_core::agent::AgentHandle;
use ratatui::backend::Backend;
use ratatui::Terminal;

pub use app::App;
pub use app_fx::Fx;

/// What the binary knows when it starts the interface.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub cwd: PathBuf,
    pub provider: String,
    pub model: String,
    /// `(provider, model)` pairs for the picker.
    pub models: Vec<(String, String)>,
    /// Every provider known (presets and configured), for the `/model` setup.
    pub providers: Vec<(String, nexo_core::ProviderConfig)>,
    /// Ring the bell when a turn that took at least this many seconds ends (0: never).
    pub notify_after: u64,
    /// Start with the vim keys on.
    pub vim: bool,
    pub theme: String,
    pub mouse: bool,
    pub show_reasoning: bool,
    /// A dim `HH:MM` on each message and reply (`[ui] timestamps`).
    pub timestamps: bool,
    pub fold_after: usize,
    /// A run of tool calls is one summary line, unfolded on demand (`[ui] group_tools`).
    pub group_tools: bool,
    /// Grey ghost text finishes the word being typed from the session's words; → takes it (`[ui] complete_words`).
    pub complete_words: bool,
    /// Ask the agent for a suggested next message when a turn ends and show it as ghost text (`[ui] suggest`).
    pub suggest: bool,
    /// `[ui.keys]`: action name -> key, over the default keys.
    pub keys: std::collections::BTreeMap<String, String>,
    /// Smooth scrolling, fades, pulses and the other motion (`[ui] animations`).
    pub animations: bool,
    /// The figure of dots on the welcome screen (`[ui] figure`).
    pub figure: nexo_core::config::Figure,
    /// How much the screen says by itself: `quiet` drops the key hints and the doubled figures (`[ui] detail`).
    pub detail: nexo_core::config::Detail,
    /// The to-do list as rows above the draft; off: a count in the status bar (`[ui] todo_strip`).
    pub todo_strip: bool,
    /// Pieces of the status bar and their order (`[ui] status_line`); empty: all, default order.
    pub status_line: Vec<String>,
    /// `[ui] status_command`: a shell command whose first line of output is the status bar; empty: the pieces.
    pub status_command: String,
    /// The first request, given on the command line (`nexo "fix the test"`): sent once the interface opens.
    pub prompt: Option<String>,
    pub session_id: Option<String>,
    /// `(id, title)` of earlier sessions here, newest first.
    pub sessions: Vec<(String, String)>,
    /// The Agent Skills found for the working directory; each answers to `/name`.
    pub skills: Vec<nexo_core::skills::Skill>,
}

impl Default for TuiOptions {
    fn default() -> TuiOptions {
        TuiOptions {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            provider: String::new(),
            model: String::new(),
            models: Vec::new(),
            providers: Vec::new(),
            notify_after: 15,
            vim: false,
            theme: "nexo".into(),
            mouse: true,
            show_reasoning: true,
            timestamps: true,
            fold_after: 12,
            group_tools: true,
            complete_words: true,
            suggest: true,
            keys: Default::default(),
            animations: true,
            figure: nexo_core::config::Figure::Galaxy,
            detail: nexo_core::config::Detail::Quiet,
            todo_strip: false,
            status_line: Vec::new(),
            status_command: String::new(),
            prompt: None,
            session_id: None,
            sessions: Vec::new(),
            skills: Vec::new(),
        }
    }
}

/// Draws one frame (the run loop and the tests share it).
pub fn draw_frame<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<()> {
    if app.force_clear {
        terminal.clear()?;
        app.force_clear = false;
    }
    terminal.draw(|f| ui::draw(f, app))?;
    Ok(())
}

/// The frame of the real screen: `draw_frame`, with the interface glyphs folded to ASCII where
/// the terminal cannot show them. (The tests use `draw_frame`: their captures do not depend on
/// the environment.)
pub fn draw_screen<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> anyhow::Result<()> {
    app.expire_status();
    if app.force_clear {
        terminal.clear()?;
        app.force_clear = false;
    }
    terminal.draw(|f| {
        ui::draw(f, app);
        if ascii::wanted() {
            ascii::fold(f.buffer_mut());
        }
    })?;
    Ok(())
}

/// A paste that came as keys, whole: into the draft as one paste.
/// Terminal events read on a thread of their own, with a level-triggered `poll`:
/// crossterm's async stream stalls on unix after the first KiB of a big paste
/// until the next key. Paused while an editor has the terminal.
struct Keys {
    rx: tokio::sync::mpsc::UnboundedReceiver<std::io::Result<crossterm::event::Event>>,
    paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Keys {
    fn spawn() -> Self {
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
            let failed = read.is_err();
            if tx.send(read).is_err() || failed {
                break;
            }
        });
        Keys { rx, paused }
    }

    async fn next(&mut self) -> Option<std::io::Result<crossterm::event::Event>> {
        self.rx.recv().await
    }

    fn pause(&self, on: bool) {
        self.paused.store(on, std::sync::atomic::Ordering::Release);
    }
}

/// SIGTERM and SIGHUP (`kill`, the window closed): the run loop ends the normal way, so the
/// terminal is given back (raw mode, alternate screen, mouse capture, bracketed paste).
/// Elsewhere it never fires.
struct Hangup {
    #[cfg(unix)]
    signals: Option<(tokio::signal::unix::Signal, tokio::signal::unix::Signal)>,
}

impl Hangup {
    fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Hangup { signals: signal(SignalKind::terminate()).ok().zip(signal(SignalKind::hangup()).ok()) }
        }
        #[cfg(not(unix))]
        Hangup {}
    }

    async fn wait(&mut self) {
        #[cfg(unix)]
        if let Some((term, hup)) = self.signals.as_mut() {
            let got = tokio::select! { t = term.recv() => t, h = hup.recv() => h };
            if got.is_some() {
                return;
            }
            // the signal driver is gone: nothing more will come
            self.signals = None;
        }
        std::future::pending::<()>().await
    }
}

fn end_paste(app: &mut App, pasting: &mut Option<(String, Instant)>) {
    if let Some((text, _)) = pasting.take() {
        event::handle(app, crossterm::event::Event::Paste(event::unmark(&text)));
    }
}

/// Reads the plan of every subscription login the providers use, now and then every ten minutes. A
/// provider on a key, or a login that cannot be read, is skipped without a word: the status bar just
/// shows nothing for it.
async fn refresh_plans(providers: Vec<(String, nexo_core::ProviderConfig)>) {
    loop {
        for (name, cfg) in &providers {
            if let Err(e) = nexo_providers::usage::fetch(name, cfg).await {
                tracing::debug!("plan usage of {name}: {}", e.message);
            }
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

/// Takes the terminal until the person quits or the agent shuts down.
pub async fn run(mut handle: AgentHandle, opts: TuiOptions) -> anyhow::Result<()> {
    let mouse = opts.mouse;
    let mut term = terminal::Term::enter(mouse)?;
    let mut app = App::new(opts, handle.commands.clone());
    // the requests sent before, kept between runs (↑ and Ctrl+R reach them)
    app.input.persist_history(nexo_core::paths::data_dir().join("prompt-history.jsonl"));
    if let Some(first) = app.opts.prompt.take() {
        app.start_with(&first);
    }
    // what is left of each subscription plan: read now and every ten minutes, and on every response
    let plans = tokio::spawn(refresh_plans(app.opts.providers.clone()));
    let mut events = Keys::spawn();
    let mut hangup = Hangup::new();
    let mut pacer = frame::Pacer::new(Instant::now(), app.opts.animations);
    frame::draw_synced(&mut term.terminal, &mut app)?;
    app.dirty = false;
    let mut title = String::new();
    // a paste coming as keys, in pieces: its text so far and when the last piece came
    let mut pasting: Option<(String, Instant)> = None;
    loop {
        // an event that comes while the clock rests gets its tick within a frame
        let resting = app.load() == frame::Load::Idle;
        let mut by_event = true;
        let wake = {
            let now = Instant::now();
            let deadline = pasting.as_ref().map(|(_, last)| *last + event::PASTE_GAP * 2);
            pacer.sleep(now, app.load(), app.dirty, deadline).map(|d| tokio::time::Instant::now() + d)
        };
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) if event::may_start_paste(&e) => {
                    // keys arriving faster than hands type are a paste (terminals without
                    // bracketed paste, the Windows console): gather them, through the pauses
                    // a big one comes with, so their Enters are new lines, not sends
                    let mut batch = vec![e];
                    let mut wait = if pasting.is_some() { event::PASTE_GAP } else { Duration::from_millis(8) };
                    while let Ok(Some(Ok(next))) = tokio::time::timeout(wait, events.next()).await {
                        batch.push(next);
                        if event::presses(&batch) >= event::PASTE_KEYS {
                            wait = event::PASTE_GAP;
                        }
                    }
                    match event::paste_burst(&batch, pasting.is_some()).filter(|_| app.takes_paste()) {
                        Some(text) => {
                            let (all, last) = pasting.get_or_insert_with(|| (String::new(), Instant::now()));
                            all.push_str(&text);
                            *last = Instant::now();
                            batch
                                .into_iter()
                                .filter(|e| !matches!(e, crossterm::event::Event::Key(_)))
                                .for_each(|e| event::handle(&mut app, e));
                        }
                        None => {
                            end_paste(&mut app, &mut pasting);
                            batch.into_iter().for_each(|e| event::handle(&mut app, e));
                        }
                    }
                }
                Some(Ok(e)) if matches!(e, crossterm::event::Event::Key(_)) => {
                    end_paste(&mut app, &mut pasting);
                    event::handle(&mut app, e);
                }
                Some(Ok(e)) => event::handle(&mut app, e),
                Some(Err(_)) | None => break,
            },
            ev = handle.events.recv() => match ev {
                Some(e) => {
                    app.handle_agent(e);
                    while let Ok(e) = handle.events.try_recv() {
                        app.handle_agent(e);
                    }
                }
                None => break,
            },
            _ = hangup.wait() => break,
            _ = nexo_providers::usage::changed().notified() => {}
            _ = async { match wake { Some(t) => tokio::time::sleep_until(t).await, None => std::future::pending().await } } => { by_event = false; }
        }
        app.sync_plan_usage();
        if app.should_quit {
            break;
        }
        if let Some(on) = app.mouse_request.take() {
            term.set_mouse(on);
            app.dirty = true;
        }
        if let Some(text) = app.bell_request.take() {
            // the bell, and a desktop notification where the terminal takes OSC 9 / 777
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = write!(out, "{}", notification(&text));
            let _ = out.flush();
        }
        if app.draft_edit_request {
            app.draft_edit_request = false;
            let path = nexo_core::paths::cache_dir().join("draft.md");
            let _ = std::fs::create_dir_all(nexo_core::paths::cache_dir());
            let written = std::fs::write(&path, app.input.full_text());
            events.pause(true);
            term.suspend();
            let outcome = written.map_err(anyhow::Error::from).and_then(|_| edit_file(&path));
            term.resume()?;
            events.pause(false);
            app.force_clear = true;
            app.dirty = true;
            match outcome.and_then(|_| Ok(std::fs::read_to_string(&path)?)) {
                Ok(text) => {
                    app.input.set_text(text.trim_end_matches('\n'));
                    app.flash("draft from the editor: Enter sends it");
                }
                Err(e) => app.notice(nexo_core::agent::Level::Error, format!("editor: {e}")),
            }
        }
        if let Some(path) = app.edit_request.take() {
            let before = std::fs::read(&path).ok();
            events.pause(true);
            term.suspend();
            let outcome = edit_file(&path);
            term.resume()?;
            events.pause(false);
            app.force_clear = true;
            app.dirty = true;
            match outcome {
                Ok(()) => app.notice(
                    nexo_core::agent::Level::Info,
                    edit_notice(&path.display().to_string(), before, std::fs::read(&path).ok()),
                ),
                Err(e) => app.notice(nexo_core::agent::Level::Error, format!("editor: {e}")),
            }
        }
        if terminal::inline() {
            inline::flush(&mut term.terminal, &mut app)?;
        }
        let now = Instant::now();
        if pasting.as_ref().is_some_and(|(_, last)| last.elapsed() >= event::PASTE_GAP * 2) {
            end_paste(&mut app, &mut pasting);
        }
        if by_event && resting {
            pacer.kick();
        }
        let t = pacer.tick(now, app.load());
        if t.run {
            app.tick_frame(t.beat);
        }
        if pacer.draw(now, app.dirty) {
            app.dirty = false;
            frame::draw_synced(&mut term.terminal, &mut app)?;
        }
        let now = app.terminal_title();
        if now != title {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(&now));
            title = now;
        }
    }
    // the inline mode writes what is left above the live zone, which is then cleared
    if terminal::inline() {
        app.busy = false;
        let _ = inline::flush(&mut term.terminal, &mut app);
        let _ = term.terminal.clear();
    }
    // --no-alt-screen: the screen is cleared and the whole conversation written, so it stays in the scrollback
    let dump = (!terminal::alt_screen() && !terminal::inline()).then(|| {
        let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
        inline::scrollback_text(&mut app, width)
    });
    if dump.is_some() {
        use crossterm::{
            cursor::MoveTo,
            terminal::{Clear, ClearType},
        };
        let _ = crossterm::execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0));
    }
    plans.abort();
    drop(term);
    let hint = app.resume_hint();
    if let Some(text) = dump {
        println!("{text}");
    }
    if let Some(h) = hint {
        println!("{h}");
    }
    Ok(())
}

/// What the trust question shows (see [`ask_trust`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAsk {
    pub path: String,
    /// One short line on what a yes enables.
    pub enables: String,
    /// Set apart when trusting the folder means more than a project (the home folder).
    pub warning: Option<String>,
}

/// Asks "Do you trust the files in this folder?" on a screen of its own, before the interface and
/// the agent start, so a yes can still shape the configuration they start from. `opts` gives the
/// theme, the mouse and the motion. True only for a yes; No, Esc, Ctrl+C, a closed window or a
/// terminal that stops answering are a no.
pub async fn ask_trust(ask: TrustAsk, opts: TuiOptions) -> anyhow::Result<bool> {
    let mut term = terminal::Term::enter(opts.mouse)?;
    let (tx, _commands) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(opts, tx);
    app.modal = Some(modal::Modal::Trust(modal::Trust::new(ask.path, ask.enables, ask.warning)));
    let mut events = Keys::spawn();
    let mut hangup = Hangup::new();
    let answer = loop {
        frame::draw_synced(&mut term.terminal, &mut app)?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(e)) => event::handle(&mut app, e),
                Some(Err(_)) | None => break false,
            },
            _ = hangup.wait() => break false,
            // the dialog pops open over a few frames
            _ = tokio::time::sleep(Duration::from_millis(33)) => {}
        }
        if let Some(yes) = app.trust_answer {
            break yes;
        }
        // closed by a click outside it: not a yes
        if app.modal.is_none() {
            break false;
        }
    };
    drop(term);
    Ok(answer)
}

/// What a long turn's end writes to the terminal: the bell, then the desktop notification in
/// the two forms terminals take (OSC 9: iTerm2, WezTerm, kitty, Windows Terminal, foot;
/// OSC 777: GNOME Terminal, Konsole and other VTE ones). A terminal that knows neither
/// ignores them. Control characters in the text are dropped so it cannot end the sequence.
pub fn notification(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    format!("\x07\x1b]9;{clean}\x07\x1b]777;notify;nexo;{clean}\x07")
}

/// What to say once the editor closed: a file left as it was is not "saved".
fn edit_notice(path: &str, before: Option<Vec<u8>>, after: Option<Vec<u8>>) -> String {
    if before == after {
        format!("{path} not changed")
    } else {
        format!("{path} saved; it applies from the next request")
    }
}

/// Opens `path` in `$VISUAL`, else `$EDITOR`, else Notepad on Windows and `nano`, else `vi`,
/// elsewhere, and waits.
fn edit_file(path: &std::path::Path) -> anyhow::Result<()> {
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

static SUPERVISE: std::sync::OnceLock<nexo_agent::supervise::Options> = std::sync::OnceLock::new();
static MAILBOX: std::sync::OnceLock<std::sync::Arc<nexo_core::mailbox::Mailbox>> = std::sync::OnceLock::new();

/// The mailbox of this process: the interface polls it, hands its deliveries to the agent and shows what
/// it brings. Unset (as in tests, or with mail off), nothing is polled.
pub fn set_mailbox(m: std::sync::Arc<nexo_core::mailbox::Mailbox>) {
    let _ = MAILBOX.set(m);
}

pub(crate) fn mailbox() -> Option<std::sync::Arc<nexo_core::mailbox::Mailbox>> {
    MAILBOX.get().cloned()
}

/// Where `/panel` keeps its audit log and listens for hooks. Unset (as in tests), it writes nothing and
/// listens nowhere.
pub fn set_supervise_options(o: nexo_agent::supervise::Options) {
    let _ = SUPERVISE.set(o);
}

pub(crate) fn supervise_options() -> nexo_agent::supervise::Options {
    SUPERVISE.get().cloned().unwrap_or_default()
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
}
