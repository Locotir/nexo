//! What the view shows: the zoom, the presentation and the source filter.

use super::*;

/// How far in the view looks. `Cards` is the normal one (it still falls back to lines when the window is
/// narrow); `-` zooms out to lines and then to blocks, `+` back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Blocks,
    List,
    Cards,
}

/// Presentation, independent of card zoom. Map falls back to tree in narrow windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum View {
    Cards,
    Tree,
    Map,
}

/// Which sessions the view lists: `s` cycles it.
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
