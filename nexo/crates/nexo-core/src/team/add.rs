//! Adding a lane to `team.toml`: the questions `nexo team add` and the `/team` window both ask, how an answer
//! is checked, and the write that keeps what the person already has in the file (comments included).

use std::path::Path;

use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

use crate::config::{team_file, Lane, TeamConfig};

/// What a person who has no lanes yet is told, wherever the lanes are listed or started: the command that makes
/// one, and what the file it writes looks like.
pub fn no_lanes_help(file: &Path) -> String {
    format!(
        "No lanes yet. Create one with: nexo team add\n\nOr write {} yourself:\n\n\
         [[lane]]\nname = \"DOCS\"\ndir = \"~/work/app\"\nevery = \"1h\"\nmode = \"accept-edits\"\n\
         routine = \"Check the open issues and report what changed.\"",
        crate::paths::shown(file)
    )
}

/// One question about a new lane, in the order they are asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Dir,
    Routine,
    Every,
    Model,
    Mode,
}

/// Every question, in the order asked.
pub const FIELDS: [Field; 6] = [Field::Name, Field::Dir, Field::Routine, Field::Every, Field::Model, Field::Mode];

impl Field {
    /// The question.
    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Dir => "Folder",
            Field::Routine => "What should the lane do",
            Field::Every => "How often",
            Field::Model => "Model",
            Field::Mode => "Permission mode",
        }
    }

    /// What goes with the question: what an empty answer means, or examples.
    pub fn hint(self) -> &'static str {
        match self {
            Field::Name => "letters, digits, - and _",
            Field::Dir => "the folder it works in",
            Field::Routine => "the routine, in a sentence or two",
            Field::Every => "15m, 1h; empty: answer mail only",
            Field::Model => "provider/model; empty: your default",
            Field::Mode => "ask, accept-edits, plan, yolo, read-only",
        }
    }

    /// What an empty answer stands for, when it stands for something.
    pub fn default(self) -> Option<String> {
        match self {
            Field::Dir => Some(std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_else(|_| ".".into())),
            Field::Mode => Some("accept-edits".into()),
            _ => None,
        }
    }

    /// Whether an empty answer is refused.
    pub fn required(self) -> bool {
        matches!(self, Field::Name | Field::Routine)
    }
}

/// Puts `answer` (empty: the default) in the lane's `field`.
pub fn set(lane: &mut Lane, field: Field, answer: &str) {
    let a = answer.trim();
    let a = if a.is_empty() { field.default().unwrap_or_default() } else { a.to_string() };
    let opt = (!a.is_empty()).then(|| a.clone());
    match field {
        Field::Name => lane.name = a,
        Field::Dir => lane.dir = a,
        Field::Routine => lane.routine = opt,
        Field::Every => lane.every = opt,
        Field::Model => lane.model = opt,
        Field::Mode => lane.mode = opt,
    }
}

/// Why the lane, as filled in so far, cannot be added next to `existing` (None: it can). Only `field`, the
/// one just answered, is held to the rules: the ones not asked yet stand in with something that passes.
pub fn problem(existing: &TeamConfig, lane: &Lane, field: Field) -> Option<String> {
    let mut l = lane.clone();
    let mut stand_in = None;
    if field != Field::Name {
        let mut name = String::from("_");
        while existing.get(&name).is_some() {
            name.push('_');
        }
        l.name = name.clone();
        stand_in = Some(name);
    }
    if field != Field::Dir && l.dir.trim().is_empty() {
        l.dir = ".".into();
    }
    if field != Field::Routine && l.routine.is_none() {
        l.routine = Some("-".into());
    }
    if field == Field::Routine && l.routine.is_none() {
        return Some("say what the lane should do".into());
    }
    let mut all = existing.clone();
    all.lane.push(l);
    let parsed = toml::to_string(&all).map_err(|e| e.to_string()).and_then(|t| TeamConfig::parse(&t));
    // The stand-in name is ours, not the person's: a message about it would name a lane they never typed.
    parsed.err().map(|e| match &stand_in {
        Some(name) => e.replace(&format!("lane {name}: "), ""),
        None => e,
    })
}

/// Adds `lane` to `<config_dir>/team.toml`, creating the file when there is none. What was in the file stays as
/// it is; the new lane is appended and the file is read back before it replaces the old one.
pub fn add(lane: &Lane) -> Result<(), String> {
    let file = team_file();
    let old = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let existing = TeamConfig::parse(&old).map_err(|e| format!("fix {} first: {e}", file.display()))?.team;
    let text = appended(&old, lane)?;
    let back = TeamConfig::parse(&text).map_err(|e| format!("{}: {e}", lane.name))?;
    if back.team.lane.len() != existing.lane.len() + 1 || back.team.get(lane.name.trim()).is_none() {
        return Err("the lane did not read back from the file; nothing was written".into());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = file.with_extension("toml.new");
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("cannot replace {}: {e}", file.display()))
}

/// `old` with `lane` as one more `[[lane]]` at the end, everything else as it was.
fn appended(old: &str, lane: &Lane) -> Result<String, String> {
    let mut doc: DocumentMut = old.parse().map_err(|e: toml_edit::TomlError| e.message().to_string())?;
    let mut t = Table::new();
    t.insert("name", value(lane.name.trim()));
    t.insert("dir", value(lane.dir.trim()));
    for (key, v) in [("model", &lane.model), ("mode", &lane.mode), ("every", &lane.every), ("routine", &lane.routine)] {
        if let Some(v) = v {
            t.insert(key, value(v.as_str()));
        }
    }
    if let Some(d) = lane.max_spend_day {
        t.insert("max_spend_day", value(d));
    }
    if !old.trim().is_empty() {
        t.decor_mut().set_prefix("\n");
    }
    let lanes = doc.entry("lane").or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    lanes.as_array_of_tables_mut().ok_or("`lane` in team.toml is not a list of [[lane]] tables")?.push(t);
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(name: &str) -> Lane {
        Lane { name: name.into(), dir: "/w".into(), routine: Some("Go.".into()), ..Lane::default() }
    }

    #[test]
    fn a_lane_is_appended_after_what_the_file_already_says() {
        let old = "# my lanes\n[[lane]]\nname = \"A\" # the first\ndir = \"/a\"\nroutine = \"x\"\n";
        let mut l = lane("B");
        l.every = Some("15m".into());
        l.mode = Some("accept-edits".into());
        l.max_spend_day = Some(2.0);
        let text = appended(old, &l).unwrap();
        assert!(text.starts_with(old), "{text}");
        assert!(text.ends_with("\n[[lane]]\nname = \"B\"\ndir = \"/w\"\nmode = \"accept-edits\"\nevery = \"15m\"\nroutine = \"Go.\"\nmax_spend_day = 2.0\n"), "{text}");
        assert_eq!(TeamConfig::parse(&text).unwrap().team.lane.len(), 2);
        assert_eq!(appended("", &lane("B")).unwrap().lines().next(), Some("[[lane]]"), "no blank line first");
    }

    #[test]
    fn a_routine_with_quotes_and_lines_reads_back_the_same() {
        let mut l = lane("Q");
        l.routine = Some("Say \"hi\".\nThen stop.".into());
        let back = TeamConfig::parse(&appended("", &l).unwrap()).unwrap().team;
        assert_eq!(back.lane[0].routine, l.routine);
    }

    #[test]
    fn an_answer_is_held_to_the_rules_of_the_file_and_the_ones_not_asked_yet_do_not_count() {
        let existing = TeamConfig { lane: vec![lane("A")] };
        let mut l = Lane::default();
        set(&mut l, Field::Name, "a");
        assert!(problem(&existing, &l, Field::Name).is_some_and(|e| e.contains("used twice")));
        set(&mut l, Field::Name, "b c");
        assert!(problem(&existing, &l, Field::Name).is_some_and(|e| e.contains("letters, digits")));
        set(&mut l, Field::Name, "B");
        assert_eq!(problem(&existing, &l, Field::Name), None, "no folder or routine asked yet");
        set(&mut l, Field::Every, "soon");
        let every = problem(&existing, &l, Field::Every).unwrap_or_default();
        assert!(every.contains("every") && !every.contains("lane _"), "{every}");
        set(&mut l, Field::Every, "");
        assert_eq!((l.every.as_deref(), problem(&existing, &l, Field::Every)), (None, None));
        set(&mut l, Field::Mode, "wild");
        let mode = problem(&existing, &l, Field::Mode).unwrap_or_default();
        assert!(mode.contains("unknown mode") && !mode.contains("lane _"), "{mode}");
        set(&mut l, Field::Mode, "");
        assert_eq!(l.mode.as_deref(), Some("accept-edits"), "an empty answer is the default");
        set(&mut l, Field::Routine, " ");
        assert!(problem(&existing, &l, Field::Routine).is_some());
    }
}
