//! The one short hint the empty draft can show besides "ask anything": a key worth knowing, said a few times
//! over a few turns and never again once the key was used. What was shown and used is kept in `hints.json` in the
//! data folder (read once when the interface starts); when the file cannot be read or written, the memory of this
//! run is all there is.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::keymap::{Action, Keymap};

/// A tip is shown at most this many times over all runs.
const MAX_SHOWN: u32 = 3;
/// Turns that must end before the first tip: a new person is not told keys in the first minutes.
const FIRST_TIP_AFTER: u64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tip {
    Search,
    History,
    Files,
    Diff,
    CopyLast,
}

impl Tip {
    const ALL: [Tip; 5] = [Tip::Search, Tip::History, Tip::Files, Tip::Diff, Tip::CopyLast];

    fn id(self) -> &'static str {
        match self {
            Tip::Search => "search",
            Tip::History => "history",
            Tip::Files => "files",
            Tip::Diff => "diff",
            Tip::CopyLast => "copy_last",
        }
    }

    /// The action whose use ends the tip (and whose key the tip names), if it has one.
    fn action(self) -> Option<Action> {
        match self {
            Tip::Search => Some(Action::Search),
            Tip::History => Some(Action::History),
            Tip::Diff => Some(Action::Diff),
            Tip::CopyLast => Some(Action::CopyLast),
            Tip::Files => None,
        }
    }

    /// The tip as one line, with the key as the person has it bound; None when the action has no key.
    pub fn text(self, keys: &Keymap) -> Option<String> {
        let key = |a: Action| Some(keys.label(a)).filter(|k| !k.is_empty());
        Some(match self {
            Tip::Search => format!("tip: {} searches the conversation", key(Action::Search)?),
            Tip::History => format!("tip: {} searches your earlier requests", key(Action::History)?),
            Tip::Files => "tip: @ picks a file to mention".to_string(),
            Tip::Diff => format!("tip: {} shows what the session changed", key(Action::Diff)?),
            Tip::CopyLast => format!("tip: {} copies the last reply", key(Action::CopyLast)?),
        })
    }
}

#[derive(Debug, Default)]
pub struct HintState {
    used: BTreeSet<String>,
    shown: BTreeMap<String, u32>,
    /// The tip last counted and the turn it was counted in: a tip stays on screen for its turn, counted once.
    counted: Option<(Tip, u64)>,
    path: Option<PathBuf>,
}

impl HintState {
    /// The state kept in `path` (empty when it is missing or unreadable); changes are written back to it.
    pub fn load(path: PathBuf) -> HintState {
        let mut s = HintState::default();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(a) = v.get("used").and_then(|u| u.as_array()) {
                    s.used = a.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                }
                if let Some(o) = v.get("shown").and_then(|u| u.as_object()) {
                    s.shown = o.iter().filter_map(|(k, n)| Some((k.clone(), n.as_u64()? as u32))).collect();
                }
            }
        }
        s.path = Some(path);
        s
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let v = serde_json::json!({ "used": self.used, "shown": self.shown });
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, v.to_string());
    }

    pub fn is_used(&self, t: Tip) -> bool {
        self.used.contains(t.id())
    }

    /// The action was used: its tip is done for good.
    pub fn used_action(&mut self, a: Action) {
        if let Some(t) = Tip::ALL.into_iter().find(|t| t.action() == Some(a)) {
            self.used_tip(t);
        }
    }

    pub fn used_tip(&mut self, t: Tip) {
        if self.used.insert(t.id().to_string()) {
            self.save();
        }
    }

    fn eligible(&self, t: Tip, turn: u64) -> bool {
        !self.is_used(t)
            && (self.shown.get(t.id()).copied().unwrap_or(0) < MAX_SHOWN || self.counted == Some((t, turn)))
    }

    /// The tip for this turn: one of the five by the turn's number, among those not used and not worn out, with a
    /// key to name. None before the third turn ends.
    pub fn tip(&self, turn: u64, keys: &Keymap) -> Option<(Tip, String)> {
        if turn < FIRST_TIP_AFTER {
            return None;
        }
        let n = Tip::ALL.len();
        (0..n)
            .map(|i| Tip::ALL[(turn as usize + i) % n])
            .filter(|t| self.eligible(*t, turn))
            .find_map(|t| t.text(keys).map(|s| (t, s)))
    }

    /// The tip was drawn whole: counted once for its turn.
    pub fn note_shown(&mut self, t: Tip, turn: u64) {
        if self.counted == Some((t, turn)) {
            return;
        }
        self.counted = Some((t, turn));
        *self.shown.entry(t.id().to_string()).or_insert(0) += 1;
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keymap {
        Keymap::default()
    }

    #[test]
    fn no_tip_in_the_first_turns_then_one_by_turn_number() {
        let s = HintState::default();
        assert!(s.tip(2, &keys()).is_none());
        let (a, _) = s.tip(3, &keys()).unwrap();
        let (b, _) = s.tip(4, &keys()).unwrap();
        assert_ne!(a, b, "the next turn says another tip");
    }

    #[test]
    fn a_used_tip_never_comes_back_and_a_worn_out_one_stops() {
        let mut s = HintState::default();
        let (t, text) = s.tip(3, &keys()).unwrap();
        assert!(text.starts_with("tip: "));
        s.used_tip(t);
        for turn in 3..30 {
            assert_ne!(s.tip(turn, &keys()).map(|x| x.0), Some(t));
        }
        let mut s = HintState::default();
        let (t, _) = s.tip(3, &keys()).unwrap();
        for turn in [3, 8, 13] {
            s.note_shown(t, turn);
            s.note_shown(t, turn);
        }
        assert_eq!(s.shown[t.id()], 3, "counted once per turn");
        assert_ne!(s.tip(18, &keys()).map(|x| x.0), Some(t), "three times is enough");
    }

    #[test]
    fn the_tip_names_the_key_as_bound_and_is_skipped_when_unbound() {
        let mut s = HintState::default();
        for t in [Tip::History, Tip::Files, Tip::Diff, Tip::CopyLast] {
            s.used_tip(t);
        }
        let (_, text) = s.tip(3, &keys()).unwrap();
        assert!(text.contains(&keys().label(Action::Search)), "{text}");
        assert!(Tip::Files.text(&keys()).unwrap().contains('@'));
    }

    #[test]
    fn state_survives_a_restart_and_a_broken_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("hints.json");
        let mut s = HintState::load(p.clone());
        s.used_action(Action::Search);
        s.note_shown(Tip::Diff, 3);
        let s2 = HintState::load(p.clone());
        assert!(s2.is_used(Tip::Search));
        assert_eq!(s2.shown["diff"], 1);
        std::fs::write(&p, "{ not json").unwrap();
        let s3 = HintState::load(p);
        assert!(!s3.is_used(Tip::Search));
    }
}
