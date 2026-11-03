//! Remappable keys: action -> key, read from `[ui.keys]`, today's keys by default.
//!
//! `[ui.keys]` maps an action name to a key (`search = "ctrl+s"`, `help = "f2"`). A key that is
//! unknown, or that two actions would share, is left out and reported (the app shows it in a toast).

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    Cancel,
    Quit,
    Search,
    FoldAll,
    Reasoning,
    Interpreted,
    CopyLast,
    History,
    Paste,
    Editor,
    Files,
    Redraw,
    PageUp,
    PageDown,
    Help,
    Mode,
    Diff,
}

impl Action {
    pub const ALL: [Action; 17] = [
        Action::Cancel,
        Action::Quit,
        Action::Search,
        Action::FoldAll,
        Action::Reasoning,
        Action::Interpreted,
        Action::CopyLast,
        Action::History,
        Action::Paste,
        Action::Editor,
        Action::Files,
        Action::Redraw,
        Action::PageUp,
        Action::PageDown,
        Action::Help,
        Action::Mode,
        Action::Diff,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::Cancel => "cancel",
            Action::Quit => "quit",
            Action::Search => "search",
            Action::FoldAll => "fold_all",
            Action::Reasoning => "reasoning",
            Action::Interpreted => "interpreted",
            Action::CopyLast => "copy_last",
            Action::History => "history",
            Action::Paste => "paste",
            Action::Editor => "editor",
            Action::Files => "files",
            Action::Redraw => "redraw",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::Help => "help",
            Action::Mode => "mode",
            Action::Diff => "diff",
        }
    }

    fn default_key(self) -> &'static str {
        match self {
            Action::Cancel => "ctrl+c",
            Action::Quit => "ctrl+d",
            Action::Search => "ctrl+f",
            Action::FoldAll => "ctrl+o",
            Action::Reasoning => "ctrl+t",
            Action::Interpreted => "ctrl+p",
            Action::CopyLast => "ctrl+y",
            Action::History => "ctrl+r",
            Action::Paste => "ctrl+v",
            Action::Editor => "ctrl+g",
            Action::Files => "ctrl+b",
            Action::Redraw => "ctrl+l",
            Action::PageUp => "pageup",
            Action::PageDown => "pagedown",
            Action::Help => "f1",
            Action::Mode => "backtab",
            Action::Diff => "f4",
        }
    }
}

/// A key as the keymap compares it: the code and whether Ctrl / Alt are held (Shift is part of
/// the code for letters and `backtab`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
}

impl Chord {
    fn of(k: &KeyEvent) -> Chord {
        let code = match k.code {
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            c => c,
        };
        Chord { code, ctrl: k.modifiers.contains(KeyModifiers::CONTROL), alt: k.modifiers.contains(KeyModifiers::ALT) }
    }
}

/// Parses `ctrl+f`, `alt+x`, `f2`, `pageup`, `backtab`, `ctrl+alt+/`.
pub fn parse_chord(spec: &str) -> Option<Chord> {
    let s = spec.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let (mut ctrl, mut alt) = (false, false);
    let mut rest = s.as_str();
    loop {
        if let Some(r) = rest.strip_prefix("ctrl+").or_else(|| rest.strip_prefix("c-")) {
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("alt+").or_else(|| rest.strip_prefix("a-")) {
            alt = true;
            rest = r;
        } else {
            break;
        }
    }
    let code = match rest {
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "pgdown" => KeyCode::PageDown,
        "backtab" | "shift+tab" => KeyCode::BackTab,
        "tab" => KeyCode::Tab,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "esc" => return None, // Esc keeps its fixed jobs
        "enter" => return None,
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            KeyCode::F(n)
        }
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?),
        _ => return None,
    };
    // a bare letter would take typing away
    if matches!(code, KeyCode::Char(_)) && !ctrl && !alt {
        return None;
    }
    Some(Chord { code, ctrl, alt })
}

#[derive(Debug, Clone)]
pub struct Keymap {
    chords: Vec<(Action, Chord)>,
}

impl Default for Keymap {
    fn default() -> Keymap {
        Keymap::from_config(&BTreeMap::new()).0
    }
}

impl Keymap {
    /// The keymap with the user's keys over the defaults, and a line for each one left out.
    pub fn from_config(keys: &BTreeMap<String, String>) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut chosen: Vec<(Action, Chord)> = Vec::new();
        for name in keys.keys() {
            if !Action::ALL.iter().any(|a| a.name() == name) {
                warnings.push(format!("keys: no action called {name}"));
            }
        }
        for a in Action::ALL {
            let spec = keys.get(a.name()).map(String::as_str);
            let chord = match spec {
                Some(s) => match parse_chord(s) {
                    Some(c) => c,
                    None => {
                        warnings.push(format!(
                            "keys: {} = {s:?} is not a usable key, keeping {}",
                            a.name(),
                            a.default_key()
                        ));
                        parse_chord(a.default_key()).expect("default key parses")
                    }
                },
                None => parse_chord(a.default_key()).expect("default key parses"),
            };
            chosen.push((a, chord));
        }
        // a key two actions share: the user's choice stays, the other goes back to its default
        for i in 0..chosen.len() {
            for j in 0..i {
                if chosen[i].1 == chosen[j].1 {
                    let (keep, redo) =
                        if keys.contains_key(chosen[i].0.name()) && !keys.contains_key(chosen[j].0.name()) {
                            (i, j)
                        } else {
                            (j, i)
                        };
                    warnings.push(format!(
                        "keys: {} and {} share a key, {} stays",
                        chosen[keep].0.name(),
                        chosen[redo].0.name(),
                        chosen[keep].0.name()
                    ));
                    let def = parse_chord(chosen[redo].0.default_key()).expect("default key parses");
                    if def == chosen[keep].1 {
                        chosen[redo].1 = Chord { code: KeyCode::Null, ctrl: false, alt: false };
                    } else {
                        chosen[redo].1 = def;
                    }
                }
            }
        }
        (Keymap { chords: chosen }, warnings)
    }

    pub fn action(&self, k: &KeyEvent) -> Option<Action> {
        let c = Chord::of(k);
        self.chords.iter().find(|(_, x)| *x == c).map(|(a, _)| *a)
    }

    /// The key an action answers to, as written in the config (for help texts).
    pub fn label(&self, a: Action) -> String {
        let c = self.chords.iter().find(|(x, _)| *x == a).map(|(_, c)| *c);
        let Some(c) = c else { return String::new() };
        let mut s = String::new();
        if c.ctrl {
            s.push_str("ctrl+");
        }
        if c.alt {
            s.push_str("alt+");
        }
        match c.code {
            KeyCode::Char(ch) => s.push(ch),
            KeyCode::F(n) => s.push_str(&format!("f{n}")),
            KeyCode::PageUp => s.push_str("pageup"),
            KeyCode::PageDown => s.push_str("pagedown"),
            KeyCode::BackTab => s.push_str("backtab"),
            KeyCode::Tab => s.push_str("tab"),
            KeyCode::Home => s.push_str("home"),
            KeyCode::End => s.push_str("end"),
            _ => {}
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn the_defaults_are_todays_keys() {
        let k = Keymap::default();
        assert_eq!(k.action(&ev(KeyCode::Char('f'), KeyModifiers::CONTROL)), Some(Action::Search));
        assert_eq!(k.action(&ev(KeyCode::F(1), KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(k.action(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)), Some(Action::Mode));
        assert_eq!(k.action(&ev(KeyCode::PageUp, KeyModifiers::NONE)), Some(Action::PageUp));
        assert_eq!(k.action(&ev(KeyCode::Char('f'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn a_remapped_key_moves_the_action() {
        let keys = BTreeMap::from([("search".to_string(), "ctrl+s".to_string())]);
        let (k, w) = Keymap::from_config(&keys);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(k.action(&ev(KeyCode::Char('s'), KeyModifiers::CONTROL)), Some(Action::Search));
        assert_eq!(k.action(&ev(KeyCode::Char('f'), KeyModifiers::CONTROL)), None);
        assert_eq!(k.label(Action::Search), "ctrl+s");
    }

    #[test]
    fn a_shared_key_is_reported_and_the_users_choice_stays() {
        let keys = BTreeMap::from([("search".to_string(), "ctrl+o".to_string())]);
        let (k, w) = Keymap::from_config(&keys);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("share a key") && w[0].contains("search stays"), "{w:?}");
        assert_eq!(k.action(&ev(KeyCode::Char('o'), KeyModifiers::CONTROL)), Some(Action::Search));
    }

    #[test]
    fn bad_keys_and_names_are_reported_and_keep_the_default() {
        let keys = BTreeMap::from([
            ("help".to_string(), "banana".to_string()),
            ("nope".to_string(), "ctrl+x".to_string()),
            ("quit".to_string(), "q".to_string()),
        ]);
        let (k, w) = Keymap::from_config(&keys);
        assert_eq!(w.len(), 3, "{w:?}");
        assert_eq!(k.action(&ev(KeyCode::F(1), KeyModifiers::NONE)), Some(Action::Help));
        assert_eq!(k.action(&ev(KeyCode::Char('d'), KeyModifiers::CONTROL)), Some(Action::Quit));
    }
}
