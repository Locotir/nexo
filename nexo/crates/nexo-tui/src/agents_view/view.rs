//! What the view shows: the groups of the tree, the kind of each row, and the two filters.

use super::*;

/// Which sessions the view lists by where they come from: `F` cycles it, and it is kept for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Every session, nexo first.
    #[default]
    All,
    /// Only this program's sessions.
    Nexo,
    /// Only the sessions of other programs.
    External,
}

impl Filter {
    pub fn next(self) -> Filter {
        match self {
            Filter::All => Filter::Nexo,
            Filter::Nexo => Filter::External,
            Filter::External => Filter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Nexo => "nexo only",
            Filter::External => "external only",
        }
    }

    pub(super) fn keeps(self, r: &AgentRow) -> bool {
        match self {
            Filter::All => true,
            Filter::Nexo => r.source == SourceKind::Nexo,
            Filter::External => r.source == SourceKind::External,
        }
    }

    pub(super) fn to_u8(self) -> u8 {
        match self {
            Filter::All => 0,
            Filter::Nexo => 1,
            Filter::External => 2,
        }
    }

    pub(super) fn from_u8(n: u8) -> Filter {
        match n {
            1 => Filter::Nexo,
            2 => Filter::External,
            _ => Filter::All,
        }
    }
}

/// Which rows the view lists by what they do: `f` cycles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Show {
    #[default]
    All,
    Working,
    NeedsYou,
}

impl Show {
    pub(super) fn next(self) -> Show {
        match self {
            Show::All => Show::Working,
            Show::Working => Show::NeedsYou,
            Show::NeedsYou => Show::All,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Show::All => "everything",
            Show::Working => "working",
            Show::NeedsYou => "needs you",
        }
    }

    pub(super) fn keeps(self, r: &AgentRow) -> bool {
        match self {
            Show::All => true,
            Show::Working => works(r),
            Show::NeedsYou => needs_you(r),
        }
    }
}

/// What a row is: it decides the keys that act on it and what the pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A `nexo team` lane, with or without a live session.
    Lane,
    /// This session.
    This,
    /// A sub-agent or job of this session (`<session>.<job id>`).
    Sub,
    /// Another live nexo session (`nexo:<id>`).
    Peer,
    /// A job of another nexo session (`nexo:<id>.<job id>`).
    Job,
    /// A session of another program.
    Other,
}

impl Kind {
    pub fn of(r: &AgentRow) -> Kind {
        if r.lane.is_some() || r.key.starts_with("lane:") {
            return Kind::Lane;
        }
        if r.source == SourceKind::External {
            return Kind::Other;
        }
        match r.key.strip_prefix("nexo:") {
            Some(rest) if rest.contains('.') => Kind::Job,
            Some(_) => Kind::Peer,
            None if r.parent.is_some() => Kind::Sub,
            None => Kind::This,
        }
    }

    pub(super) fn word(self) -> &'static str {
        match self {
            Kind::Lane => "lane",
            Kind::This => "this session",
            Kind::Sub => "sub-agent",
            Kind::Peer => "nexo session",
            Kind::Job => "job",
            Kind::Other => "session",
        }
    }
}

/// A group of the tree, in the order they are drawn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Needs,
    This,
    Lanes,
    Nexo,
    /// The sessions of another program, by its name.
    Program(String),
}

impl Group {
    pub(super) fn of(kind: Kind, r: &AgentRow) -> Group {
        match kind {
            Kind::Lane => Group::Lanes,
            Kind::This | Kind::Sub => Group::This,
            Kind::Peer | Kind::Job => Group::Nexo,
            Kind::Other => Group::Program(r.program.clone()),
        }
    }

    pub(super) fn title(&self) -> String {
        match self {
            Group::Needs => "NEEDS YOU".into(),
            Group::This => "THIS SESSION".into(),
            Group::Lanes => "LANES".into(),
            Group::Nexo => "NEXO SESSIONS".into(),
            Group::Program(p) => p.to_uppercase(),
        }
    }

    /// Other programs start folded to one line with their count.
    pub(super) fn folded_at_first(&self) -> bool {
        matches!(self, Group::Program(_))
    }

    pub(super) fn id(&self) -> String {
        match self {
            Group::Program(p) => format!("program:{p}"),
            g => g.title(),
        }
    }
}

/// Whether a row needs the person: a permission or a question waits, it is stuck or looping, its lane failed,
/// or mail for it waits for the person's approval.
pub fn needs_you(r: &AgentRow) -> bool {
    waits(r)
        || (r.state == State::Looping && unheld(r))
        || r.lane.as_ref().is_some_and(|l| l.state == "failed")
        || r.held > 0
}

/// Whether a lane row's own state speaks for it. A lane without a live runner keeps whatever its last
/// observation left (its spend note reads as work), so only its lane word counts; a row of no lane always speaks.
fn unheld(r: &AgentRow) -> bool {
    r.lane.as_ref().is_none_or(|l| l.runner)
}

/// Whether a row works now: its turn runs, or its lane's round does.
pub(super) fn works(r: &AgentRow) -> bool {
    r.lane.as_ref().is_some_and(|l| l.state == "running") || (r.state == State::Working && unheld(r))
}

/// Whether a row waits on the person or looks stuck, as its own state says (a lane without a runner waits on
/// nothing).
pub(super) fn waits(r: &AgentRow) -> bool {
    attention(r.state) && unheld(r)
}

/// What the status bar's fleet chip counts: lanes that are on, this session's sub-agents at work, other live
/// sessions, and rows that need the person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FleetCount {
    pub lanes: usize,
    pub subs: usize,
    pub sessions: usize,
    pub need: usize,
}

impl FleetCount {
    /// The counts of the panel's rows.
    pub fn of(rows: &[AgentRow]) -> FleetCount {
        let mut c = FleetCount::default();
        for r in rows {
            let live = r.state != State::Done;
            match Kind::of(r) {
                Kind::Lane => c.lanes += usize::from(r.lane.as_ref().is_some_and(|l| l.runner || l.state == "running")),
                Kind::Sub => c.subs += usize::from(live),
                Kind::Peer | Kind::Other => c.sessions += usize::from(live),
                Kind::This | Kind::Job => {}
            }
            c.need += usize::from(needs_you(r));
        }
        c
    }

    /// Nothing besides this session is live, and it runs no sub-agent: the chip stays off.
    pub fn is_empty(&self) -> bool {
        self.lanes + self.subs + self.sessions + self.need == 0
    }

    /// `◉ 2 lanes · 3 subs · 1 needs you`, the parts that are not zero.
    pub fn text(&self, ascii: bool) -> String {
        let n = |k: usize, one: &str, many: &str| (k > 0).then(|| format!("{k} {}", if k == 1 { one } else { many }));
        let parts: Vec<String> = [
            n(self.lanes, "lane", "lanes"),
            n(self.subs, "sub", "subs"),
            n(self.sessions, "session", "sessions"),
            n(self.need, "needs you", "need you"),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!("{} {}", if ascii { "o" } else { "◉" }, parts.join(" · "))
    }
}
