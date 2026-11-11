//! The boxes of a block's lines: which rows are the contents of a code block, a tool's output,
//! a diff, a file view, a thinking body, a table or a plan, and which rows are one logical line
//! that the layout wrapped. Computed with the lines, so a copy takes the content and not the screen.

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Code,
    Output,
    Diff,
    File,
    Think,
    Table,
    Plan,
}

/// A row that continues the logical line above it: `gap` spaces the wrap trimmed at the break,
/// and `deco` columns of prefix (rail, hanging indent) in front of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cont {
    pub line: u32,
    pub gap: u8,
    pub deco: u16,
}

/// Rows `start..=end` of a block, `deco` columns of decoration on the left of each.
#[derive(Debug, Clone)]
pub struct Region {
    pub kind: Kind,
    pub start: u32,
    pub end: u32,
    pub deco: u16,
    /// The original text of each row (row `start + k` is `src[k]`), when it maps one to one.
    pub src: Option<Arc<Vec<String>>>,
    /// Rows inside that are headings of the box, not content (a diff's hunk headers).
    pub skip: Vec<u32>,
}

impl Region {
    pub fn new(kind: Kind, start: usize, end: usize, deco: usize) -> Region {
        Region { kind, start: start as u32, end: end as u32, deco: deco as u16, src: None, skip: Vec::new() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    /// Sorted by `start`, never overlapping.
    pub regions: Vec<Region>,
    /// Sorted by `line`.
    pub conts: Vec<Cont>,
    /// Columns of blank margin before every row.
    pub margin: u16,
    /// The first row ends with a right-aligned time stamp span.
    pub stamped: bool,
}

impl Meta {
    /// The region that holds row `line`.
    pub fn region_of(&self, line: usize) -> Option<usize> {
        let i = self.regions.partition_point(|r| r.start as usize <= line).checked_sub(1)?;
        (self.regions[i].end as usize >= line).then_some(i)
    }

    pub fn cont_of(&self, line: usize) -> Option<Cont> {
        let i = self.conts.binary_search_by_key(&(line as u32), |c| c.line).ok()?;
        Some(self.conts[i])
    }

    /// Adds `cols` of prefix to every row's decoration.
    pub fn add_deco(&mut self, cols: usize) {
        for r in &mut self.regions {
            r.deco += cols as u16;
        }
        for c in &mut self.conts {
            c.deco += cols as u16;
        }
    }

    /// Keeps the first `n` rows.
    pub fn truncate(&mut self, n: usize) {
        self.conts.retain(|c| (c.line as usize) < n);
        self.regions.retain(|r| (r.start as usize) < n);
        for r in &mut self.regions {
            r.end = r.end.min(n.saturating_sub(1) as u32);
        }
    }

    /// Appends `other`, whose rows now start at row `at`.
    pub fn append(&mut self, other: Meta, at: usize) {
        for mut r in other.regions {
            r.start += at as u32;
            r.end += at as u32;
            for s in &mut r.skip {
                *s += at as u32;
            }
            self.regions.push(r);
        }
        for mut c in other.conts {
            c.line += at as u32;
            self.conts.push(c);
        }
    }

    /// Drops the first `n` rows (blank rows a renderer cut off).
    pub fn drop_front(&mut self, n: usize) {
        self.conts.retain(|c| c.line as usize >= n);
        for c in &mut self.conts {
            c.line -= n as u32;
        }
        self.regions.retain(|r| r.end as usize >= n);
        for r in &mut self.regions {
            for s in &mut r.skip {
                *s = s.saturating_sub(n as u32);
            }
            r.start = r.start.saturating_sub(n as u32);
            r.end -= n as u32;
        }
    }

    /// Shifts every row down by `n` (rows were put above).
    pub fn shift(&mut self, n: usize) {
        let all = std::mem::take(self);
        self.append(all, n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_region_of_a_row() {
        let mut m = Meta::default();
        m.regions.push(Region::new(Kind::Code, 2, 4, 3));
        m.regions.push(Region::new(Kind::Table, 7, 8, 0));
        assert_eq!(m.region_of(1), None);
        assert_eq!(m.region_of(3), Some(0));
        assert_eq!(m.region_of(5), None);
        assert_eq!(m.region_of(8), Some(1));
        m.truncate(4);
        assert_eq!(m.regions.len(), 1);
        assert_eq!(m.regions[0].end, 3);
        m.shift(2);
        assert_eq!(m.region_of(4), Some(0));
    }
}
