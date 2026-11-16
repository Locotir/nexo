//! The run loop: takes the terminal until the person quits or the agent shuts down.

use std::time::{Duration, Instant};

use nexo_core::agent::AgentHandle;

use crate::terminal_io::{edit_file, edit_notice, notification, notify_command, Hangup, Keys};
use crate::{event, frame, hints, inline, terminal, App, TuiOptions};

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
    app.hint_state = hints::HintState::load(nexo_core::paths::data_dir().join("hints.json"));
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
            let term_program = std::env::var("TERM_PROGRAM").ok();
            match term_program
                .as_deref()
                .filter(|_| cfg!(target_os = "macos"))
                .and_then(|t| notify_command(Some(t), &text))
            {
                // Terminal.app and VS Code ignore both sequences: the bell, and the system notification
                Some((program, args)) => {
                    let _ = write!(out, "\x07");
                    let _ = std::process::Command::new(program)
                        .args(args)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        // reaped by a thread of its own, so no zombie is left behind
                        .map(|mut c| std::thread::spawn(move || drop(c.wait())));
                }
                None => {
                    let _ = write!(out, "{}", notification(&text));
                }
            }
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
