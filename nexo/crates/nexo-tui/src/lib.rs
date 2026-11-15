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
pub mod fx;
pub mod hints;
pub mod inline;
pub mod input;
pub mod insights;
pub mod keymap;
pub mod links;
pub mod modal;
pub mod motion;
pub mod overlay;
pub mod palette;
pub mod palette_key;
pub mod regions;
pub mod render;
pub mod replay;
pub mod scrollbar;
pub mod select;
pub mod status_cmd;
pub mod terminal;
pub mod theme;
pub mod toast;
pub mod todo_strip;
pub mod trace;
pub mod ui;
pub mod views;

mod globals;
mod run;
mod terminal_io;
mod trust;

use std::path::PathBuf;

use ratatui::backend::Backend;
use ratatui::Terminal;

// The effects and views moved into folders; the old flat names stay valid for other crates and tests.
pub use app::App;
pub use app_fx::Fx;
pub use fx::app as app_fx;
pub use fx::burst as fx_burst;
pub use fx::chrome as fx_chrome;
pub use fx::diff as fx_diff;
pub use fx::effort as fx_effort;
pub use fx::galaxy as fx_galaxy;
pub use fx::goal as fx_goal;
pub use fx::home as fx_home;
pub use fx::panel as panel_fx;
pub use fx::starfield as fx_starfield;
pub use fx::stars as fx_stars;
pub use fx::stream as fx_stream;
pub(crate) use globals::{mailbox, supervise_options};
pub use globals::{set_mailbox, set_supervise_options};
pub use run::run;
pub use terminal_io::notification;
pub use trust::{ask_trust, TrustAsk};
pub use views::chrome as chrome_view;
pub use views::goal as goal_view;
pub use views::jobs as jobs_view;
pub use views::mcp as mcp_view;
pub use views::sent as sent_view;
pub use views::todo as todo_view;

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
    /// One short hint in the empty draft: queued messages, or a key worth knowing (`[ui] hints`).
    pub hints: bool,
    /// `[ui.keys]`: action name -> key, over the default keys.
    pub keys: std::collections::BTreeMap<String, String>,
    /// Smooth scrolling, fades, pulses and the other motion (`[ui] animations`).
    pub animations: bool,
    /// The figure of dots on the welcome screen (`[ui] figure`).
    pub figure: nexo_core::config::Figure,
    /// Faint stars drifting across an empty session (`[ui] home_stars`).
    pub home_stars: bool,
    /// A burst of dots when a goal is met or a turn fails (`[ui] bursts`).
    pub bursts: bool,
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
            hints: true,
            keys: Default::default(),
            animations: true,
            figure: nexo_core::config::Figure::Galaxy,
            home_stars: true,
            bursts: true,
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
