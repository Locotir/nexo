//! Where the cards go: the layout of a body of a given size, the groups and the strips of the body.

use super::*;

/// The cards or the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Cards {
        cols: u16,
        card_w: u16,
    },
    List,
    /// The far zoom: a colour block per session.
    Blocks {
        cols: u16,
    },
}

/// The sessions of one program, under one header.
#[derive(Debug, Clone)]
pub(super) struct Group {
    pub(super) label: String,
    pub(super) source: SourceKind,
    /// Indices into the shown rows; empty for a placeholder that says there is nothing yet.
    pub(super) idx: Vec<usize>,
}

/// One horizontal strip of the body: a header, a row of cards (or one line), or a group's empty line.
#[derive(Debug, Clone)]
pub(super) enum Band {
    Header(usize),
    Row(Vec<usize>),
    Empty(usize),
}

/// The blocks of the far zoom in a body `w` wide: as many columns as fit.
pub fn blocks_layout(w: u16) -> Layout {
    Layout::Blocks { cols: ((w + GAP) / (BLOCK_W + GAP)).clamp(1, MAX_BLOCK_COLS) }
}

/// How `n` sessions lay out in a body `w` wide and `h` tall. Pure, so the sizes are testable.
pub fn cards_layout(w: u16, h: u16) -> Layout {
    if w < LIST_BELOW || h < CARD_H {
        return Layout::List;
    }
    let cols = ((w + GAP) / (CARD_MIN_W + GAP)).clamp(1, MAX_COLS);
    let card_w = ((w - GAP * (cols - 1)) / cols).min(CARD_MAX_W);
    Layout::Cards { cols, card_w }
}

/// The sessions balanced over their rows: 4 cards on 3 columns make two rows of 2, not 3 and an orphan.
pub(super) fn balanced(n: usize, cols: usize) -> usize {
    let cols = cols.max(1);
    if n <= cols {
        return n.max(1);
    }
    n.div_ceil(n.div_ceil(cols))
}

/// `n` cells cut into `k` runs that differ by at most one, the longer ones first.
pub(super) fn split_evenly(n: u16, k: usize) -> Vec<u16> {
    let k = k.max(1) as u16;
    (0..k).map(|i| n / k + u16::from(i < n % k)).collect()
}

impl AgentsView {
    /// The strips of the body for `cols` cards across.
    pub(super) fn bands(groups: &[Group], cols: usize, balance: bool) -> Vec<Band> {
        let mut out = Vec::new();
        for (g, grp) in groups.iter().enumerate() {
            out.push(Band::Header(g));
            if grp.idx.is_empty() {
                out.push(Band::Empty(g));
            }
            let per = if balance { balanced(grp.idx.len(), cols) } else { cols.max(1) };
            for chunk in grp.idx.chunks(per) {
                out.push(Band::Row(chunk.to_vec()));
            }
        }
        out
    }

    /// Cards across, the height of one, and whether the strips sit tight (lines) or breathe.
    pub(super) fn shape(l: Layout, card_h: u16) -> (usize, u16, bool) {
        match l {
            Layout::Cards { cols, .. } => (cols as usize, card_h, false),
            Layout::Blocks { cols } => (cols as usize, BLOCK_H, false),
            Layout::List => (1, 1, true),
        }
    }

    pub(super) fn band_h(b: &Band, item_h: u16) -> u16 {
        match b {
            Band::Row(_) => item_h,
            _ => 1,
        }
    }

    /// The blank rows between two strips: none in the lines, none under a header, one elsewhere.
    pub(super) fn band_gap(prev: &Band, tight: bool) -> u16 {
        u16::from(!tight && !matches!(prev, Band::Header(_)))
    }

    /// The height of strips `from..to` laid one under the other.
    pub(super) fn span(bands: &[Band], from: usize, to: usize, item_h: u16, tight: bool) -> u16 {
        let mut h = 0;
        for i in from..to.min(bands.len()) {
            if i > from {
                h += Self::band_gap(&bands[i - 1], tight);
            }
            h += Self::band_h(&bands[i], item_h);
        }
        h
    }

    /// The height of all the strips of `groups` laid out as `l`.
    pub(super) fn content_h(groups: &[Group], l: Layout, card_h: u16) -> u16 {
        let (cols, item_h, tight) = Self::shape(l, card_h);
        let bands = Self::bands(groups, cols, matches!(l, Layout::Cards { .. }));
        Self::span(&bands, 0, bands.len(), item_h, tight)
    }

    /// Cards that would hide sessions behind a scroll give way to the lines when those show every session.
    pub(super) fn fit_cards(groups: &[Group], l: Layout, body_h: u16, card_h: u16) -> Layout {
        match l {
            Layout::Cards { .. }
                if Self::content_h(groups, l, card_h) > body_h
                    && Self::content_h(groups, Layout::List, card_h) <= body_h =>
            {
                Layout::List
            }
            other => other,
        }
    }
}
