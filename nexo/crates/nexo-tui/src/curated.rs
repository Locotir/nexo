//! What the curator added to a model call beyond the conversation, as rows of kind, name and size
//! (never the text), for `/context` and the `/context-live` panel.

use nexo_core::{CuratedItem, CuratedKind, CuratedState};
use ratatui::text::{Line, Span};

use crate::render::blocks::fmt_k;
use crate::render::wrap::{truncate, width};
use crate::theme::Theme;

/// How many model calls keep their rows.
pub const CALLS: usize = 8;

/// The text shown when a call carries nothing from the curator.
pub const NOTHING: &str = "No curator items reported for this selection.";

const DISCLAIMER: &str = "RACE selection before final fit; not proof of what was sent.";

fn kind_name(k: CuratedKind) -> &'static str {
    match k {
        CuratedKind::Recalled => "recalled",
        CuratedKind::Start => "start",
        CuratedKind::Rule => "rule",
        CuratedKind::Thread => "thread",
        CuratedKind::Goal => "goal",
        CuratedKind::Memory => "memory",
    }
}

/// Recovery reported by the history map, restricted to identified inactive topics.
/// Sizes are original source sizes: a recalled cell does not report the size of its excerpt.
/// Mixed messages are classified by the map kind, not by individual message parts.
pub fn recovery_lines(
    theme: &Theme,
    cells: &[nexo_core::ContextCell],
    zones: &[nexo_core::ContextZone],
    w: usize,
) -> Vec<Line<'static>> {
    use nexo_core::{CellKind, CellState};
    if cells.is_empty() {
        return Vec::new();
    }
    let mut counts = [0u64; 4];
    let mut sizes = [0u64; 4];
    let mut topics = std::collections::BTreeSet::new();
    let mut unidentified = 0u64;
    for cell in cells.iter().filter(|c| c.state == CellState::Recalled) {
        let Some(zone) = zones.get(cell.zone as usize) else {
            unidentified = unidentified.saturating_add(1);
            continue;
        };
        if zone.active {
            continue;
        }
        topics.insert(cell.zone);
        let i = match cell.kind {
            CellKind::Request => 0,
            CellKind::Reply => 1,
            CellKind::Call => 2,
            CellKind::Output => 3,
        };
        counts[i] = counts[i].saturating_add(1);
        sizes[i] = sizes[i].saturating_add(cell.tokens);
    }
    let mut out =
        crate::render::wrap::wrap_plain("Old-topic recovery · map evidence", w.max(10), theme.bold(theme.fg), &[]);
    let summary = format!("{} recalled sources · {} inactive topics", counts.iter().sum::<u64>(), topics.len());
    out.extend(crate::render::wrap::wrap_plain(&summary, w.max(10), theme.dim(), &[]));
    for ((label, count), size) in
        ["user messages", "assistant replies", "tool calls", "tool outputs"].into_iter().zip(counts).zip(sizes)
    {
        let text = format!("{label}: {count} · ~{} original tok", fmt_k(size));
        out.extend(crate::render::wrap::wrap_plain(&text, w.max(10), theme.dim(), &[]));
    }
    if unidentified > 0 {
        let text = format!("{unidentified} recalled sources: topic unknown (excluded)");
        out.extend(crate::render::wrap::wrap_plain(&text, w.max(10), theme.dim2(), &[]));
    }
    out.extend(crate::render::wrap::wrap_plain(
        "Map kinds classify mixed messages. Original sizes are not recovered payload sizes. Zero means no mapped evidence, not no recovery. See /sent for the request.",
        w.max(10),
        theme.dim2(),
        &[],
    ));
    out
}

/// The items in (or trimmed) when `kept`, else the ones left out; biggest first, ties by name.
pub fn sorted(items: &[CuratedItem], kept: bool) -> Vec<&CuratedItem> {
    let mut v: Vec<&CuratedItem> = items.iter().filter(|i| (i.state != CuratedState::Out) == kept).collect();
    v.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.label.cmp(&b.label)));
    v
}

/// The tokens the call carries from the curator.
pub fn total(items: &[CuratedItem]) -> u64 {
    sorted(items, true).iter().map(|i| i.tokens).sum()
}

/// `s` cut in the middle to `w` cells, so a path keeps its start and its file name.
fn truncate_mid(s: &str, w: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    if width(s) <= w || w < 5 {
        return truncate(s, w);
    }
    let gs: Vec<&str> = s.graphemes(true).collect();
    let (mut head, mut tail) = (String::new(), String::new());
    let (mut hw, mut tw) = (0, 0);
    let keep = w - 1;
    let tail_max = keep - keep / 3;
    for g in gs.iter().rev() {
        if tw + width(g) > tail_max {
            break;
        }
        tail.insert_str(0, g);
        tw += width(g);
    }
    for g in &gs {
        if hw + width(g) + tw > keep {
            break;
        }
        head.push_str(g);
        hw += width(g);
    }
    format!("{head}…{tail}")
}

fn row(theme: &Theme, i: &CuratedItem, w: usize) -> Line<'static> {
    const KIND: usize = 9;
    let size = format!("{}{}", if i.state == CuratedState::Trimmed { "~" } else { "" }, fmt_k(i.tokens));
    let name_w = w.saturating_sub(KIND + 2 + 7 + 2).max(4);
    let name = if i.label.contains('/') { truncate_mid(&i.label, name_w) } else { truncate(&i.label, name_w) };
    let pad = " ".repeat(name_w.saturating_sub(crate::render::wrap::width(&name)));
    Line::from(vec![
        Span::styled(format!("{:<KIND$}", kind_name(i.kind)), theme.dim()),
        Span::raw(format!("  {name}{pad}  ")),
        Span::styled(format!("{size:>7}"), theme.dim()),
    ])
}

/// The section: a title, what the call carries (biggest first) with the total, then what was left out.
pub fn lines(theme: &Theme, items: &[CuratedItem], w: usize, title: &str) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(title.to_string(), theme.bold(theme.fg)))];
    if items.is_empty() {
        out.extend(crate::render::wrap::wrap_plain(NOTHING, w.max(10), theme.dim(), &[]));
        out.extend(crate::render::wrap::wrap_plain(DISCLAIMER, w.max(10), theme.dim(), &[]));
        return out;
    }
    let kept = sorted(items, true);
    for i in &kept {
        out.push(row(theme, i, w));
    }
    if !kept.is_empty() {
        let n = kept.iter().map(|i| i.tokens).sum::<u64>();
        out.push(Line::from(vec![
            Span::styled(format!("{:<9}", "total"), theme.bold(theme.fg)),
            Span::raw(format!("  {:<width$}  ", "", width = w.saturating_sub(9 + 2 + 7 + 2).max(4))),
            Span::styled(format!("{:>7}", fmt_k(n)), theme.bold(theme.fg)),
        ]));
        if kept.iter().any(|i| i.state == CuratedState::Trimmed) {
            out.push(Line::from(Span::styled("~ trimmed by curator", theme.dim2())));
        }
    }
    let left = sorted(items, false);
    if !left.is_empty() {
        out.push(Line::default());
        out.push(Line::from(Span::styled("Left out", theme.bold(theme.fg))));
        for i in left {
            out.push(row(theme, i, w));
        }
    }
    out.extend(crate::render::wrap::wrap_plain(DISCLAIMER, w.max(10), theme.dim(), &[]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(kind: CuratedKind, label: &str, tokens: u64, state: CuratedState) -> CuratedItem {
        CuratedItem { kind, label: label.into(), tokens, state }
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn rows_are_biggest_first_with_a_total_and_the_left_out_after() {
        let items = vec![
            it(CuratedKind::Rule, "never touch main", 300, CuratedState::In),
            it(CuratedKind::Thread, "parser rewrite", 12_400, CuratedState::In),
            it(CuratedKind::Recalled, "src/lex.rs", 900, CuratedState::Trimmed),
            it(CuratedKind::Memory, "old notes", 2_000, CuratedState::Out),
        ];
        let t: Vec<String> = lines(&Theme::dark(), &items, 60, "Added").iter().map(text).collect();
        assert!(t[1].starts_with("thread") && t[1].contains("12.4k"), "{t:?}");
        assert!(t[2].starts_with("recalled") && t[2].contains("~900"), "{t:?}");
        assert!(t[3].starts_with("rule"), "{t:?}");
        assert!(t[4].starts_with("total") && t[4].contains("13.6k"), "{t:?}");
        let k = t.iter().position(|l| l == "Left out").expect("left out header");
        assert!(t[k + 1].starts_with("memory") && t[k + 1].contains("2.0k"), "{t:?}");
        assert_eq!(total(&items), 13_600);
    }

    #[test]
    fn wide_labels_keep_the_size_column_aligned() {
        use crate::render::wrap::width;
        let items = vec![
            it(CuratedKind::Recalled, "src/ascii.rs", 900, CuratedState::In),
            it(CuratedKind::Thread, "リファクタリング計画メモ", 800, CuratedState::In),
            it(CuratedKind::Memory, "📝📝📝📝📝📝📝📝📝📝", 700, CuratedState::Out),
        ];
        for w in [24, 40] {
            let t: Vec<String> = lines(&Theme::dark(), &items, w, "Added").iter().map(text).collect();
            let rows: Vec<&String> =
                t.iter().filter(|l| ["recalled", "thread", "memory"].iter().any(|kind| l.starts_with(kind))).collect();
            assert!(rows.len() >= 3, "{t:?}");
            let first = width(rows[0]);
            for r in rows {
                assert_eq!(width(r), first, "w={w} {t:?}");
            }
        }
    }

    #[test]
    fn a_long_path_keeps_its_file_name() {
        let items =
            vec![it(CuratedKind::Recalled, "/home/someone/projects/app/src/parser/lexer.rs", 900, CuratedState::In)];
        let t: Vec<String> = lines(&Theme::dark(), &items, 34, "Added").iter().map(text).collect();
        assert!(t[1].contains("lexer.rs") && t[1].contains('…'), "{t:?}");
        let items =
            vec![it(CuratedKind::Recalled, "read /home/someone/projects/app/src/lexer.rs ×3", 900, CuratedState::Out)];
        let t: Vec<String> = lines(&Theme::dark(), &items, 44, "Added").iter().map(text).collect();
        assert!(t.iter().any(|l| l.contains("lexer.rs ×3")), "{t:?}");
    }

    #[test]
    fn recovery_counts_each_kind_only_in_identified_inactive_topics() {
        use nexo_core::{CellKind, CellState, ContextCell, ContextZone};
        let zones = vec![
            ContextZone { label: "old topic".into(), active: false, card: true },
            ContextZone { label: "current topic".into(), active: true, card: false },
            ContextZone { label: "another old topic".into(), active: false, card: false },
        ];
        let mut cells: Vec<_> = [CellKind::Request, CellKind::Reply, CellKind::Call, CellKind::Output]
            .into_iter()
            .map(|kind| ContextCell { kind, tokens: 700, state: CellState::Recalled, zone: 0 })
            .collect();
        cells.extend([
            ContextCell { kind: CellKind::Output, tokens: 300, state: CellState::Recalled, zone: 2 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Recalled, zone: 1 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Whole, zone: 0 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Unknown, zone: 0 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Shrunk, zone: 0 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Dropped, zone: 0 },
            ContextCell { kind: CellKind::Output, tokens: 9000, state: CellState::Recalled, zone: 99 },
        ]);
        let t: Vec<_> = recovery_lines(&Theme::dark(), &cells, &zones, 160).iter().map(text).collect();
        assert!(t.iter().any(|s| s == "5 recalled sources · 2 inactive topics"), "{t:?}");
        for kind in ["user messages", "assistant replies", "tool calls"] {
            assert!(t.iter().any(|s| s == &format!("{kind}: 1 · ~700 original tok")), "{t:?}");
        }
        assert!(t.iter().any(|s| s == "tool outputs: 2 · ~1.0k original tok"), "{t:?}");
        assert!(t.iter().any(|s| s.contains("1 recalled sources: topic unknown (excluded)")), "{t:?}");
        assert!(t.iter().any(|s| s.contains("not recovered payload sizes")), "{t:?}");
    }

    #[test]
    fn recovery_without_topic_metadata_is_unknown_and_narrow_lines_wrap() {
        use nexo_core::{CellKind, CellState, ContextCell};
        assert!(recovery_lines(&Theme::dark(), &[], &[], 30).is_empty());
        let cells = [ContextCell { kind: CellKind::Output, tokens: 1000, state: CellState::Recalled, zone: 0 }];
        let t: Vec<_> = recovery_lines(&Theme::dark(), &cells, &[], 160).iter().map(text).collect();
        assert!(t.iter().any(|s| s == "0 recalled sources · 0 inactive topics"), "{t:?}");
        assert!(t.iter().any(|s| s.contains("topic unknown (excluded)")), "{t:?}");
        for line in &recovery_lines(&Theme::dark(), &cells, &[], 30) {
            assert!(width(&text(line)) <= 30, "{line:?}");
        }
    }

    #[test]
    fn nothing_says_so() {
        let t: Vec<String> = lines(&Theme::dark(), &[], 100, "Added").iter().map(text).collect();
        assert_eq!(t, vec!["Added".to_string(), NOTHING.to_string(), DISCLAIMER.to_string()]);
    }
}
