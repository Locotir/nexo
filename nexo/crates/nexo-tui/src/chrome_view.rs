//! `/chrome`: the way into the person's own browser in a small window. A checklist (✓ or ○, in plain words) of
//! what is in place, and the keys that do the next step: `s` sets nexo up (what `nexo chrome-setup` does), `c`
//! copies the folder to pick in "Load unpacked", `r` looks again (it also looks by itself every two seconds until a
//! browser answers) and `u` tells the agent to use the browser that answered.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::{AgentCommand, ChromeFiles, ChromeHostState, ChromeInfo};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::wrap::{width, wrap_plain};
use crate::theme::Theme;

/// How often the window looks again while no browser answers.
const RECHECK: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ChromeView {
    /// What the agent last said; `None` until the first answer.
    pub info: Option<ChromeInfo>,
    pub scroll: usize,
    /// Asked, not answered yet: no second question goes out.
    waiting: bool,
    last_asked: Instant,
    /// The agent was told to use this browser (id).
    told: Option<String>,
}

impl ChromeView {
    pub fn new(now: Instant) -> ChromeView {
        ChromeView { info: None, scroll: 0, waiting: true, last_asked: now, told: None }
    }

    /// The agent answered.
    pub fn set(&mut self, info: ChromeInfo) {
        self.info = Some(info);
        self.waiting = false;
    }

    /// Whether a new look is due at `now`: nothing is pending, the last one is two seconds old and no browser
    /// answers yet (once one does, the ✓ stays and nothing more is asked until `r`).
    pub fn due(&mut self, now: Instant) -> bool {
        let none = self.info.as_ref().is_some_and(|i| i.running.is_empty());
        if self.waiting || !none || now.duration_since(self.last_asked) < RECHECK {
            return false;
        }
        self.mark_asked(now);
        true
    }

    fn mark_asked(&mut self, now: Instant) {
        self.waiting = true;
        self.last_asked = now;
    }

    /// The folder to pick in "Load unpacked", as text.
    fn folder(&self) -> Option<String> {
        self.info.as_ref().map(|i| i.extension_dir.display().to_string())
    }

    pub fn key(&mut self, key: KeyEvent, now: Instant) -> ModalOut {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => ModalOut::Close,
            KeyCode::Char('s') => ModalOut::Send(AgentCommand::ChromeSetup),
            KeyCode::Char('c') => match self.folder() {
                Some(f) => ModalOut::Copy(f),
                None => ModalOut::Nothing,
            },
            KeyCode::Char('r') => {
                self.mark_asked(now);
                ModalOut::Send(AgentCommand::ChromeCheck)
            }
            KeyCode::Char('u') => match self.info.as_ref().and_then(|i| i.running.first()) {
                Some(b) => {
                    let id = b.id.clone();
                    self.told = Some(id.clone());
                    ModalOut::Send(AgentCommand::ChromeUse { id })
                }
                None => ModalOut::Nothing,
            },
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                ModalOut::Nothing
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll += 1;
                ModalOut::Nothing
            }
            _ => ModalOut::Nothing,
        }
    }

    /// What the window's size follows: the shape of the checklist (which notes and steps show), not the polling.
    pub fn size_key(&self) -> (bool, bool, bool, usize, bool, bool) {
        match &self.info {
            None => (false, false, false, 0, false, false),
            Some(i) => (true, steps_shown(i), host_ok(i), i.running.len(), i.in_use.is_some(), self.told.is_some()),
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }

    pub fn title(&self) -> String {
        match &self.info {
            Some(i) if !i.running.is_empty() => " your browser · connected ".into(),
            _ => " your browser ".into(),
        }
    }

    /// The keys, in the row pinned under the body; the longest set that fits `w`.
    pub fn keys(&self, w: usize, theme: &Theme) -> Line<'static> {
        let can_use = self.info.as_ref().is_some_and(|i| !i.running.is_empty() && i.in_use.is_none());
        let mut long: Vec<(&str, &str)> = vec![("s", "set up"), ("c", "copy folder"), ("r", "check again")];
        let mut short: Vec<(&str, &str)> = vec![("s", "set up"), ("c", "copy"), ("r", "check")];
        let mut tiny: Vec<(&str, &str)> = vec![("s", ""), ("c", ""), ("r", "")];
        if can_use {
            long.push(("u", "use it here"));
            short.push(("u", "use"));
            tiny.push(("u", ""));
        }
        long.push(("Esc", "close"));
        short.push(("Esc", ""));
        tiny.push(("Esc", ""));
        for set in [long, short, tiny] {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled(k.to_string(), theme.style(theme.accent)));
                if !what.is_empty() {
                    spans.push(Span::styled(format!(" {what}"), theme.dim()));
                }
            }
            if spans.iter().map(|s| width(&s.content)).sum::<usize>() <= w {
                return Line::from(spans);
            }
        }
        Line::from(Span::styled("Esc close", theme.dim()))
    }

    /// The body at `w` columns.
    pub fn lines(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let Some(info) = &self.info else {
            return vec![Line::from(Span::styled("looking…", theme.dim()))];
        };
        let mut lines = Vec::new();
        let check = |lines: &mut Vec<Line<'static>>, ok: bool, text: String| {
            let (mark, style) = if ok { ("✓ ", theme.style(theme.success)) } else { ("○ ", theme.dim()) };
            let mut rows = wrap_plain(&text, w.saturating_sub(2), Default::default(), &[]).into_iter();
            if let Some(mut first) = rows.next() {
                first.spans.insert(0, Span::styled(mark, style));
                lines.push(first);
            }
            for mut r in rows {
                r.spans.insert(0, Span::raw("  "));
                lines.push(r);
            }
        };
        check(&mut lines, host_ok(info), host_text(info));
        check(&mut lines, info.files == ChromeFiles::Current, files_text(info));
        check(&mut lines, !info.running.is_empty(), running_text(info));
        let using = info.in_use.is_some();
        check(&mut lines, using || self.told.is_some(), use_text(info, self.told.as_deref()));
        if steps_shown(info) {
            lines.push(Line::default());
            let head = Style::from_bold(theme);
            lines.push(Line::from(Span::styled("In your browser, on chrome://extensions:", head)));
            let folder = info.extension_dir.display();
            for step in [
                "1. Turn on Developer mode (top right).".to_string(),
                format!("2. Click Load unpacked and pick {folder}"),
                "3. Click the nexo button in the toolbar.".to_string(),
            ] {
                for r in wrap_plain(&step, w.saturating_sub(2), Default::default(), &[]) {
                    let mut r = r;
                    r.spans.insert(0, Span::raw("  "));
                    lines.push(r);
                }
            }
            lines.push(Line::from(Span::styled(
                "  Press c to copy the folder. It is checked here by itself.",
                theme.dim(),
            )));
        } else if !host_ok(info) {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("Press s: nexo sets itself up and opens your browser.", theme.dim())));
        }
        lines
    }
}

/// A short helper so the bold heading reads the same everywhere in this window.
struct Style;
impl Style {
    fn from_bold(theme: &Theme) -> ratatui::style::Style {
        theme.style(theme.fg).add_modifier(Modifier::BOLD)
    }
}

fn host_ok(i: &ChromeInfo) -> bool {
    !i.hosts.is_empty() && i.hosts.iter().any(|h| h.state == ChromeHostState::Ready)
}

fn names(i: &ChromeInfo, want: impl Fn(&ChromeHostState) -> bool) -> String {
    i.hosts.iter().filter(|h| want(&h.state)).map(|h| h.browser.as_str()).collect::<Vec<_>>().join(", ")
}

fn host_text(i: &ChromeInfo) -> String {
    let ready = names(i, |s| *s == ChromeHostState::Ready);
    if !ready.is_empty() {
        let old = i.hosts.iter().find(|h| matches!(h.state, ChromeHostState::OldPath(_)));
        return match old {
            Some(o) => format!("nexo is registered for {ready}; {} points to an old nexo: press s", o.browser),
            None => format!("nexo is registered for {ready}"),
        };
    }
    if let Some((b, p)) = i.hosts.iter().find_map(|h| match &h.state {
        ChromeHostState::OldPath(p) => Some((h.browser.as_str(), p.as_str())),
        _ => None,
    }) {
        return format!("nexo is registered for {b} but points to an old path ({p}): press s");
    }
    format!("nexo is not registered for {} yet", names(i, |_| true))
}

fn files_text(i: &ChromeInfo) -> String {
    let d = i.extension_dir.display();
    match i.files {
        ChromeFiles::Current => format!("the extension files are in {d} and match this nexo"),
        ChromeFiles::Stale => format!("the extension files in {d} are from another nexo version: press s"),
        ChromeFiles::Missing => format!("the extension files are not written yet (they go to {d})"),
    }
}

fn running_text(i: &ChromeInfo) -> String {
    if i.running.is_empty() {
        return "no browser with the extension answers yet".into();
    }
    let list: Vec<String> = i.running.iter().map(|b| format!("{} (id {})", b.name, b.id)).collect();
    format!("{} answers", list.join(", "))
}

fn use_text(i: &ChromeInfo, told: Option<&str>) -> String {
    match (&i.in_use, told) {
        (Some(d), _) => format!("this session is using {d}"),
        (None, Some(id)) => {
            format!("this session is not using it yet; the agent was told to use {id} and you click Allow when it asks")
        }
        (None, None) if !i.running.is_empty() => "this session is not using it: press u".into(),
        (None, None) => "this session is not using it".into(),
    }
}

/// The three steps show while the browser has to be told about the extension.
fn steps_shown(i: &ChromeInfo) -> bool {
    i.running.is_empty() && (i.just_set_up || (host_ok(i) && i.files != ChromeFiles::Missing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::agent::{ChromeHostInfo, ChromeRunning};
    use std::path::PathBuf;

    fn info(host: ChromeHostState, files: ChromeFiles) -> ChromeInfo {
        ChromeInfo {
            hosts: vec![ChromeHostInfo { browser: "Google Chrome".into(), state: host }],
            extension_dir: PathBuf::from("/data/chrome-extension"),
            files,
            running: Vec::new(),
            not_answering: Vec::new(),
            in_use: None,
            just_set_up: false,
        }
    }

    fn text(v: &ChromeView) -> String {
        let theme = Theme::dark();
        v.lines(80, &theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn view(i: ChromeInfo) -> ChromeView {
        let mut v = ChromeView::new(Instant::now());
        v.set(i);
        v
    }

    #[test]
    fn nothing_set_up_is_four_open_circles_and_the_way_to_start() {
        let t = text(&view(info(ChromeHostState::Missing, ChromeFiles::Missing)));
        assert_eq!(t.matches('○').count(), 4, "{t}");
        assert!(!t.contains('✓'), "{t}");
        assert!(t.contains("nexo is not registered for Google Chrome yet"), "{t}");
        assert!(t.contains("not written yet (they go to /data/chrome-extension)"), "{t}");
        assert!(t.contains("Press s"), "{t}");
        assert!(!t.contains("Load unpacked"), "no steps before set up: {t}");
    }

    #[test]
    fn manifest_only_ticks_the_host_and_shows_the_steps_with_the_real_folder() {
        let t = text(&view(info(ChromeHostState::Ready, ChromeFiles::Missing)));
        assert!(t.starts_with("✓ nexo is registered for Google Chrome"), "{t}");
        assert_eq!(t.matches('✓').count(), 1, "{t}");
        // files missing: the steps wait for the set up
        assert!(!t.contains("Load unpacked"), "{t}");
        let mut i = info(ChromeHostState::Ready, ChromeFiles::Current);
        i.just_set_up = true;
        let t = text(&view(i));
        assert_eq!(t.matches('✓').count(), 2, "{t}");
        assert!(t.contains("2. Click Load unpacked and pick /data/chrome-extension"), "{t}");
        assert!(t.contains("1. Turn on Developer mode") && t.contains("3. Click the nexo button"), "{t}");
    }

    #[test]
    fn an_old_path_is_said_and_not_ticked() {
        let t = text(&view(info(ChromeHostState::OldPath("/old/nexo".into()), ChromeFiles::Stale)));
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(t.contains("points to an old path (/old/nexo): press s"), "{t}");
        assert!(t.contains("from another nexo version"), "{t}");
        assert!(!t.contains('✓'), "{t}");
    }

    #[test]
    fn connected_ticks_three_and_offers_to_use_it_in_the_session() {
        let mut i = info(ChromeHostState::Ready, ChromeFiles::Current);
        i.running = vec![ChromeRunning { id: "chrome-1".into(), name: "Google Chrome".into() }];
        let mut v = view(i.clone());
        let t = text(&v);
        assert_eq!(t.matches('✓').count(), 3, "{t}");
        assert!(t.contains("Google Chrome (id chrome-1) answers"), "{t}");
        assert!(t.contains("this session is not using it: press u"), "{t}");
        assert!(!t.contains("Load unpacked"), "{t}");
        // `u` sends the id and the line says the agent was told
        let out = v.key(KeyEvent::from(KeyCode::Char('u')), Instant::now());
        assert!(matches!(out, ModalOut::Send(AgentCommand::ChromeUse { id }) if id == "chrome-1"));
        assert_eq!(text(&v).matches('✓').count(), 4);
        // in use by the session
        i.in_use = Some("your own Google Chrome (Linux), id chrome-1".into());
        let t = text(&view(i));
        assert_eq!(t.matches('✓').count(), 4, "{t}");
        assert!(t.contains("this session is using your own Google Chrome"), "{t}");
    }

    #[test]
    fn it_looks_again_every_two_seconds_until_a_browser_answers() {
        let t0 = Instant::now();
        let mut v = ChromeView::new(t0);
        assert!(!v.due(t0 + Duration::from_secs(5)), "the first answer has not come");
        v.set(info(ChromeHostState::Ready, ChromeFiles::Current));
        assert!(!v.due(t0 + Duration::from_secs(1)));
        assert!(v.due(t0 + Duration::from_secs(3)));
        assert!(!v.due(t0 + Duration::from_secs(6)), "one question at a time");
        let mut i = info(ChromeHostState::Ready, ChromeFiles::Current);
        i.running = vec![ChromeRunning { id: "c".into(), name: "Google Chrome".into() }];
        v.set(i);
        assert!(!v.due(t0 + Duration::from_secs(60)), "a browser answers: nothing more is asked");
    }

    #[test]
    fn keys_copy_the_folder_and_set_up() {
        let mut v = view(info(ChromeHostState::Missing, ChromeFiles::Missing));
        let now = Instant::now();
        assert!(
            matches!(v.key(KeyEvent::from(KeyCode::Char('c')), now), ModalOut::Copy(p) if p == "/data/chrome-extension")
        );
        assert!(matches!(v.key(KeyEvent::from(KeyCode::Char('s')), now), ModalOut::Send(AgentCommand::ChromeSetup)));
        assert!(matches!(v.key(KeyEvent::from(KeyCode::Char('r')), now), ModalOut::Send(AgentCommand::ChromeCheck)));
        assert!(matches!(v.key(KeyEvent::from(KeyCode::Char('u')), now), ModalOut::Nothing), "nothing to use yet");
        assert!(matches!(v.key(KeyEvent::from(KeyCode::Esc), now), ModalOut::Close));
    }
}
