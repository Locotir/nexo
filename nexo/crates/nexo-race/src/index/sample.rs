//! Read complete source lines, never a synthetic join of pieces. Keep the original prefix
//! allowance and add a small tail only where the prefix stopped. The single delimiter count
//! needed for absolute coordinates is bounded at ingest; lookup never recounts the gap.
use super::{Index, Post, UNIT_LINES};
use crate::facts::LINE_CHARS;
use crate::mechanical::MARK;

const HEAD_BYTES: usize = 256 * 1024;
const TAIL_BYTES: usize = 32 * 1024;
const TAIL_LINES: usize = 512;
const COORD_BYTES: usize = 4 * 1024 * 1024;

fn prefix(text: &str) -> &str {
    if text.len() <= HEAD_BYTES {
        text
    } else {
        let end = text.as_bytes()[..HEAD_BYTES].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        &text[..end]
    }
}

fn quotable(line: &str) -> bool {
    !line.is_empty() && line.len() <= 4 * LINE_CHARS && line.chars().count() <= LINE_CHARS && !line.contains(MARK)
}

impl Index {
    pub fn add_relation(&mut self, unit: usize, text: &str) {
        let head = prefix(text);
        let mut ks = Vec::new();
        let (mut end, mut lines) = (0, 0);
        for raw in head.split_inclusive('\n').take(UNIT_LINES) {
            let line = raw.strip_suffix('\n').map_or(raw, |s| s.strip_suffix('\r').unwrap_or(s));
            if quotable(line) {
                self.add_line((unit as u32, lines), line, &mut ks);
            }
            end += raw.len();
            lines += 1;
        }
        if end == text.len() || text.len() > COORD_BYTES {
            return;
        }
        // Start at a real line boundary, after the head (even if its last line was empty).
        let bytes = text.as_bytes();
        let mut start = end.max(text.len().saturating_sub(TAIL_BYTES));
        if start > 0 && bytes[start - 1] != b'\n' {
            let Some(n) = bytes[start..].iter().position(|&b| b == b'\n') else { return };
            start += n + 1;
        }
        let tail = &text[start..];
        let skip = tail.lines().count().saturating_sub(TAIL_LINES);
        start += tail.split_inclusive('\n').take(skip).map(str::len).sum::<usize>();
        if start == text.len() {
            return;
        }
        let first = lines + bytes[end..start].iter().filter(|&&b| b == b'\n').count() as u32;
        self.tails.insert(unit as u32, (first, start as u32));
        for (i, line) in text[start..].lines().enumerate().filter(|(_, l)| quotable(l)) {
            self.add_line((unit as u32, first + i as u32), line, &mut ks);
        }
    }

    pub fn relation_line<'a>(&self, (unit, line): Post, text: &'a str) -> Option<&'a str> {
        let (block, n) = match self.tails.get(&unit) {
            Some(&(first, start)) if line >= first => (text.get(start as usize..)?, line - first),
            _ => (prefix(text), line),
        };
        block.lines().nth(n as usize).filter(|l| quotable(l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::key;

    #[test]
    fn line_limited_head_and_tail_stay_disjoint_and_ordered() {
        let raw = "éclair\r\n".repeat(UNIT_LINES + TAIL_LINES + 8);
        let mut ix = Index::default();
        ix.add_relation(2, &raw);
        let hits = &ix.posts[&key("éclair")];
        assert_eq!(hits.len(), UNIT_LINES + TAIL_LINES);
        assert!(hits.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(hits[UNIT_LINES], (2, (UNIT_LINES + 8) as u32));
        assert_eq!(ix.relation_line(hits[UNIT_LINES], &raw), Some("éclair"));
    }

    #[test]
    fn giant_partial_lines_are_not_search_hits_and_the_coordinate_scan_has_a_ceiling() {
        let huge = "ø".repeat(HEAD_BYTES);
        let mut ix = Index::default();
        let raw = format!("start\r\n{huge} partialreceipt\r\nend\r\n");
        ix.add_relation(0, &raw);
        assert_eq!(ix.lines, 2);
        assert!(!ix.posts.contains_key(&key("partialreceipt")));
        assert_eq!(ix.relation_line((0, 2), &raw), Some("end"));
        let raw = format!("start\n{}\nend", "x".repeat(COORD_BYTES));
        ix.add_relation(1, &raw);
        assert!(!ix.tails.contains_key(&1));
        assert_eq!(ix.posts[&key("end")], [(0, 2)]);
    }
}
