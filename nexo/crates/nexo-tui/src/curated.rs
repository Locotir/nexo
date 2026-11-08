//! What the curator added to a model call beyond the conversation, as rows of kind, name and size
//! (never the text), for `/context` and the `/context-live` panel.

use nexo_core::{CuratedItem, CuratedKind, CuratedState};
use ratatui::text::{Line, Span};

use crate::render::blocks::fmt_k;
use crate::render::wrap::truncate;
use crate::theme::Theme;

/// How many model calls keep their rows.
pub const CALLS: usize = 8;

/// The text shown when a call carries nothing from the curator.
pub const NOTHING: &str = "The curator added nothing to this call.";

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

fn row(theme: &Theme, i: &CuratedItem, w: usize) -> Line<'static> {
    const KIND: usize = 9;
    let size = format!("{}{}", if i.state == CuratedState::Trimmed { "~" } else { "" }, fmt_k(i.tokens));
    let name_w = w.saturating_sub(KIND + 2 + 7 + 2).max(4);
    let name = truncate(&i.label, name_w);
    let pad = " ".repeat(name_w.saturating_sub(name.chars().count()));
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
            out.push(Line::from(Span::styled("~ trimmed to fit", theme.dim2())));
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
    fn nothing_says_so() {
        let t: Vec<String> = lines(&Theme::dark(), &[], 60, "Added").iter().map(text).collect();
        assert_eq!(t, vec!["Added".to_string(), NOTHING.to_string()]);
    }
}
