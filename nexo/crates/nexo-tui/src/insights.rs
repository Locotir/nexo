//! "What's contributing to your usage?": the section under the figures of the Usage tab.
//!
//! The sessions on this machine are read once, off the interface thread, when the tab is first
//! shown (and again, quietly, each time the window is opened: unchanged files come from the
//! scanner's cache). Until the result is in the section says so and shows no figures. Changing the
//! period never reads anything: the result holds all three. The reading and the ranking are
//! `nexo_agent::session_scan`; this file is the state and the drawing.

use std::sync::mpsc::{channel, Receiver, TryRecvError};

use nexo_agent::session_scan::{self as scan, Insight, InsightKind, Insights, Period};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::overlay::bar;
use crate::render::blocks::fmt_k;
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::theme::Theme;

/// From this inner width a row has a bar.
pub const BARS_FROM: usize = 44;
/// Cells of a row's bar.
const BAR_CELLS: usize = 8;

/// A finished scan: the three periods, ranked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ranked {
    /// In the order of [`Period::ALL`].
    pub by_period: [Insights; 3],
    pub partial: Option<String>,
}

impl Ranked {
    pub fn of(report: &scan::Report) -> Ranked {
        Ranked { by_period: Period::ALL.map(|p| scan::rank(report, p)), partial: report.partial() }
    }

    pub fn get(&self, p: Period) -> &Insights {
        &self.by_period[Period::ALL.iter().position(|q| *q == p).unwrap_or(1)]
    }
}

/// What the section knows.
pub struct Scanner {
    rx: Option<Receiver<Result<Ranked, String>>>,
    ranked: Option<Ranked>,
    error: Option<String>,
    /// A newer reading is wanted (the window was opened again); what is shown stays until it comes.
    stale: bool,
    /// Counts the results, so a window can tell its size is out of date.
    pub version: u64,
    /// Where the sessions are; none: the person's own.
    dir: Option<std::path::PathBuf>,
}

impl Default for Scanner {
    fn default() -> Scanner {
        Scanner { rx: None, ranked: None, error: None, stale: true, version: 0, dir: None }
    }
}

/// What to draw.
pub enum View<'a> {
    Scanning,
    Failed(&'a str),
    Ready(&'a Ranked),
}

impl Scanner {
    /// A scanner of the sessions in `dir` (none: the person's own).
    pub fn in_dir(dir: Option<std::path::PathBuf>) -> Scanner {
        Scanner { dir, ..Scanner::default() }
    }

    /// A scanner that already holds a result (tests, and a demo).
    pub fn with(ranked: Ranked) -> Scanner {
        Scanner { ranked: Some(ranked), stale: false, version: 1, ..Scanner::default() }
    }

    pub fn running(&self) -> bool {
        self.rx.is_some()
    }

    /// The window was opened: read again when the tab is shown.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    /// Starts a reading when one is wanted and none runs.
    pub fn ensure(&mut self) {
        if self.rx.is_some() || !self.stale {
            return;
        }
        self.stale = false;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let dir = self.dir.clone();
        let spawned = std::thread::Builder::new().name("usage-scan".into()).spawn(move || {
            let (now, offset) = scan::clock();
            let report = match dir {
                None => scan::scan_sessions(now, offset),
                Some(d) => scan::scan(&d, now, offset, &mut scan::Cache::default()),
            };
            let _ = tx.send(report.map(|r| Ranked::of(&r)));
        });
        if spawned.is_err() {
            self.rx = None;
            self.error = Some("cannot start the reading".into());
        }
    }

    /// Takes the result when it is in; true when the figures changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(Ok(r)) => {
                self.ranked = Some(r);
                self.error = None;
            }
            Ok(Err(e)) => {
                self.ranked = None;
                self.error = Some(e);
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.error = Some("the reading stopped".into());
            }
        }
        self.rx = None;
        self.version += 1;
        true
    }

    pub fn view(&self) -> View<'_> {
        match (&self.ranked, &self.error) {
            (Some(r), _) => View::Ready(r),
            (None, Some(e)) => View::Failed(e),
            (None, None) => View::Scanning,
        }
    }
}

/// The colour of a share: orange from half, amber from a quarter, the accent below.
fn share_color(theme: &Theme, share: f64) -> Color {
    if share >= 0.5 {
        theme.warn2
    } else if share >= 0.25 {
        theme.warn
    } else {
        theme.accent
    }
}

/// The person's home folder.
pub fn home() -> Option<String> {
    std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok().filter(|h| !h.is_empty())
}

/// `path` with the home folder as `~`.
pub fn tilde(path: &str, home: Option<&str>) -> String {
    nexo_core::paths::shown_from(std::path::Path::new(path), home.map(std::path::Path::new))
}

/// `s` cut to `w` columns from the middle, the cut marked: the end of a path says most.
pub fn cut_middle(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w < 3 {
        return truncate(s, w);
    }
    let chars: Vec<char> = s.chars().collect();
    let tail = (w - 1) / 2;
    let head = w - 1 - tail;
    let mut out: String = chars.iter().take(head).collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    // wide characters may still overflow
    if width(&out) > w {
        return truncate(s, w);
    }
    out
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The words of a row: the headline, a money figure for it, the lines under it, and whether it is good news.
struct Words {
    head: String,
    money: Option<f64>,
    detail: Vec<String>,
    advice: Option<&'static str>,
    good: bool,
}

fn words(i: &Insight, home: Option<&str>, room: usize) -> Words {
    let mut w = Words { head: String::new(), money: None, detail: Vec::new(), advice: None, good: false };
    match &i.kind {
        InsightKind::LongContext { sessions, calls } => {
            w.head = "Long context".into();
            w.detail.push(format!(
                "{} over {}",
                plural(*sessions as u64, "session", "sessions"),
                fmt_k(scan::LONG_CTX)
            ));
            w.detail.push(plural(*calls, "call", "calls"));
            w.detail = vec![format!("{} · {}", w.detail[0], w.detail[1])];
            w.advice = Some("Start fresh or let the curator set more aside.");
        }
        InsightKind::CacheMisses { misses, tokens, rebuilds } => {
            w.head = "Cache misses".into();
            w.detail.push(format!("{} · {} tokens", plural(*misses, "re-send", "re-sends"), fmt_k(*tokens)));
            if *rebuilds > 0 {
                w.detail.push(format!("{} not counted", plural(*rebuilds, "expected rebuild", "expected rebuilds")));
            }
            w.advice = Some("Pauses past the cache lifetime or edits early in the prompt re-send everything.");
        }
        InsightKind::ToolOutput { tool, top, runs } => {
            w.head = format!("{} output", cap(tool));
            w.detail.push(match top {
                Some(t) => format!("top: {t} · {}", plural(*runs, "run", "runs")),
                None => plural(*runs, "run", "runs"),
            });
            w.advice = Some(if tool == "bash" {
                "Pipe through head or tail, or let the curator trim it."
            } else {
                "Ask for less at a time, or let the curator trim it."
            });
        }
        InsightKind::Model { model, cost, dearer } => {
            w.head = format!("of spend on {model}");
            w.money = Some(*cost);
            if *dearer {
                w.advice = Some("Use a cheaper model for routine steps.");
            }
        }
        InsightKind::Project { cwd, cost } => {
            let path = tilde(cwd, home);
            w.head = format!("of spend in {}", cut_middle(&path, room.saturating_sub(14).max(8)));
            w.money = Some(*cost);
        }
        InsightKind::Reasoning { tokens } => {
            w.head = "Reasoning of all output".into();
            w.detail.push(format!("{} tokens", fmt_k(*tokens)));
            w.advice = Some("Lower the effort for simple work.");
        }
        InsightKind::SideCalls { cost } => {
            w.head = "Sub-agents and side calls".into();
            w.money = Some(*cost);
            w.advice = Some("Smaller model for sub-agents; shorter briefs.");
        }
        InsightKind::CuratorLeftOut { tokens } => {
            w.head = format!("Curator left out {}", fmt_k(*tokens));
            w.good = true;
        }
    }
    w
}

fn rule(theme: &Theme, w: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(w.min(60)), theme.dim2()))
}

/// The selector row: `today  [7 days]  30 days`, the keys at the right when there is room.
fn selector(theme: &Theme, period: Period, w: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for p in Period::ALL {
        let on = p == period;
        let label = if on { format!("[{}]", p.label()) } else { p.label().to_string() };
        used += width(&label) + 2;
        let style = if on { theme.bold(theme.accent) } else { theme.dim() };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw("  "));
    }
    let keys = "d w m";
    if used + keys.len() < w {
        spans.push(Span::raw(" ".repeat(w - used - keys.len())));
        spans.push(Span::styled(keys, theme.dim2()));
    }
    Line::from(spans)
}

/// The section's lines at `w` columns; `t` (0..1) is how far the bars have filled in; `home` is the
/// person's home folder, shown as `~` in paths.
pub fn lines(
    theme: &Theme,
    view: &View<'_>,
    period: Period,
    w: usize,
    t: f64,
    home: Option<&str>,
) -> Vec<Line<'static>> {
    let narrow = w < BARS_FROM;
    let mut out = vec![rule(theme, w)];
    out.push(Line::from(Span::styled(truncate("What's contributing to your usage?", w), theme.bold(theme.fg))));
    let sub = if narrow { "Approximate · this machine only" } else { "Approximate · sessions on this machine only" };
    out.push(Line::from(Span::styled(truncate(sub, w), theme.dim())));
    let ranked = match view {
        View::Scanning => {
            out.push(Line::default());
            out.push(Line::from(Span::styled("Scanning local sessions…", theme.dim())));
            return out;
        }
        View::Failed(e) => {
            out.push(Line::default());
            out.extend(wrap_plain(e, w, theme.style(theme.warn), &[]));
            return out;
        }
        View::Ready(r) => r,
    };
    out.push(selector(theme, period, w));
    out.push(Line::default());
    let ins = ranked.get(period);
    if ins.sessions == 0 {
        let none = match period {
            Period::Day => "No sessions today on this machine.".to_string(),
            p => format!("No sessions in the last {} on this machine.", p.label()),
        };
        out.extend(wrap_plain(&none, w, Style::default(), &[]));
        let longer = Period::ALL.into_iter().find(|p| p.days() > period.days() && ranked.get(*p).sessions > 0);
        if let Some(p) = longer {
            out.push(Line::from(Span::styled(format!("Try {} ({}).", p.label(), p.key()), theme.dim())));
        }
        if let Some(p) = &ranked.partial {
            out.push(Line::from(Span::styled(truncate(p, w), theme.dim())));
        }
        return out;
    }
    let totals = &ins.totals;
    let mut head = vec![Span::styled(plural(ins.sessions as u64, "session", "sessions"), theme.dim())];
    head.push(Span::styled(format!(" · {}", plural(totals.calls, "call", "calls")), theme.dim()));
    if totals.cost > 0.0 {
        head.push(Span::styled(" · ", theme.dim()));
        head.push(Span::styled(format!("${:.2}", totals.cost), theme.style(theme.success)));
    }
    out.push(Line::from(head));
    out.push(Line::default());
    if ins.rows.is_empty() {
        out.extend(wrap_plain(
            "Nothing stands out: no single thing is over 10% of your usage.",
            w,
            theme.style(theme.success),
            &[],
        ));
    }
    for row in &ins.rows {
        out.extend(row_lines(theme, row, w, t, home));
    }
    if !ins.top.is_empty() {
        out.push(Line::default());
        let right = 14usize;
        let left = w.saturating_sub(right);
        let mut head = vec![Span::styled(format!("{:<left$}", "Biggest sessions"), theme.bold(theme.fg))];
        head.push(Span::styled(format!("{:>8}{:>6}", "cost", "cache"), theme.dim()));
        if w >= right + 8 {
            out.push(Line::from(head));
        }
        for s in &ins.top {
            // the folder gives way, the day stays
            let day = format!(" · {}", s.when);
            let room = left.saturating_sub(1);
            let folder = truncate(&crate::render::wrap::one_line(&s.project), room.saturating_sub(width(&day)).max(1));
            let title = truncate(&format!("{folder}{day}"), room);
            let pad = left.saturating_sub(width(&title));
            let mut spans = vec![Span::raw(title), Span::raw(" ".repeat(pad))];
            spans.push(Span::styled(format!("{:>8}", format!("${:.2}", s.cost)), theme.style(theme.success)));
            match s.cache_hit {
                Some(h) => {
                    spans.push(Span::styled(format!("{:>5}%", h), theme.style(theme.level_color(100 - h.min(100)))))
                }
                None => spans.push(Span::styled(format!("{:>6}", "n/a"), theme.dim())),
            }
            out.push(Line::from(spans));
        }
    }
    let mut notes: Vec<Line<'static>> = Vec::new();
    if !ins.rows.is_empty() {
        let overlap = if narrow { "Shares overlap." } else { "Shares overlap; they do not add up to 100%." };
        notes.push(Line::from(Span::styled(overlap, theme.dim())));
    }
    if totals.unpriced > 0 {
        notes.extend(wrap_plain(
            &format!("{} had no price: money is a floor.", plural(totals.unpriced, "call", "calls")),
            w,
            theme.dim(),
            &[],
        ));
    }
    if let Some(p) = &ranked.partial {
        notes.push(Line::from(Span::styled(truncate(p, w), theme.dim())));
    }
    if !notes.is_empty() {
        out.push(Line::default());
        out.extend(notes);
    }
    out
}

fn row_lines(theme: &Theme, row: &Insight, w: usize, t: f64, home: Option<&str>) -> Vec<Line<'static>> {
    let narrow = w < BARS_FROM;
    let pct = format!("{:>3}%", row.percent().min(100));
    // bar, space, percent, two spaces: the text starts here
    let indent = if narrow { pct.len() + 2 } else { BAR_CELLS + 1 + pct.len() + 2 };
    let room = w.saturating_sub(indent);
    let wd = words(row, home, room);
    let colour = if wd.good { theme.success } else { share_color(theme, row.share) };
    let mut first: Vec<Span<'static>> = Vec::new();
    if !narrow {
        let (fill, rest) = bar(row.share * t, BAR_CELLS);
        first.push(Span::styled(fill, Style::default().fg(colour)));
        first.push(Span::styled(rest, theme.dim2()));
        first.push(Span::raw(" "));
    }
    first.push(Span::styled(pct, theme.bold(colour)));
    first.push(Span::raw("  "));
    let pad = Span::raw(" ".repeat(indent));
    let money = wd.money.map(|m| format!("  ${m:.2}"));
    let money_w = money.as_ref().map_or(0, |m| width(m));
    let head_room = room.saturating_sub(money_w).max(1);
    let mut head = wrap_plain(&wd.head, head_room + indent, Style::default(), std::slice::from_ref(&pad));
    // the headline's first line carries the bar; it takes the prefix instead of the pad
    let mut out = Vec::new();
    if let Some(mut l) = head.drain(..1).next().map(|l| l.spans) {
        l.remove(0);
        first.extend(l);
        if let Some(m) = money {
            first.push(Span::styled(m, theme.style(theme.success)));
        }
        out.push(Line::from(first));
    }
    out.extend(head);
    for d in &wd.detail {
        out.extend(wrap_plain(d, w, theme.dim(), std::slice::from_ref(&pad)));
    }
    if let Some(a) = wd.advice {
        out.extend(wrap_plain(a, w, theme.dim(), &[pad]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent::session_scan::{SessionRow, Tally};

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    fn theme() -> Theme {
        Theme::dark()
    }

    fn sample() -> Ranked {
        let rows = vec![
            Insight { share: 0.62, kind: InsightKind::LongContext { sessions: 4, calls: 31 } },
            Insight {
                share: 0.41,
                kind: InsightKind::ToolOutput { tool: "bash".into(), top: Some("cargo test".into()), runs: 18 },
            },
            Insight { share: 0.24, kind: InsightKind::CacheMisses { misses: 23, tokens: 310_000, rebuilds: 2 } },
            Insight { share: 0.80, kind: InsightKind::Model { model: "model-x".into(), cost: 4.10, dearer: true } },
            Insight { share: 0.55, kind: InsightKind::Project { cwd: "/home/me/foo".into(), cost: 2.2 } },
            Insight { share: 0.22, kind: InsightKind::CuratorLeftOut { tokens: 1_200_000 } },
        ];
        let top = vec![
            SessionRow {
                id: "a".into(),
                project: "login".into(),
                when: "today".into(),
                cost: 1.82,
                cache_hit: Some(71),
            },
            SessionRow {
                id: "b".into(),
                project: "exporter".into(),
                when: "yesterday".into(),
                cost: 0.94,
                cache_hit: Some(88),
            },
            SessionRow {
                id: "c".into(),
                project: "a-folder-name-that-is-far-too-long-for-the-column".into(),
                when: "Oct 23".into(),
                cost: 0.1,
                cache_hit: None,
            },
        ];
        let totals = Tally { calls: 120, cost: 5.12, unpriced: 3, ..Tally::default() };
        let week = Insights { rows, top, totals, sessions: 9 };
        let empty = Insights::default();
        let nothing = Insights { sessions: 2, totals: Tally { calls: 4, ..Tally::default() }, ..Insights::default() };
        Ranked { by_period: [empty, week, nothing], partial: Some("partial: 400 of 512 sessions".into()) }
    }

    fn render(view: &View, period: Period, w: usize, t: f64) -> Vec<String> {
        lines(&theme(), view, period, w, t, Some("/home/me")).iter().map(text).collect()
    }

    #[test]
    fn no_line_is_wider_than_the_window() {
        let r = sample();
        for w in [30usize, 36, 40, 43, 44, 48, 60, 96] {
            for p in Period::ALL {
                for view in [
                    View::Ready(&r),
                    View::Scanning,
                    View::Failed("cannot read /a/very/long/path/to/the/sessions: permission denied"),
                ] {
                    for l in render(&view, p, w, 1.0) {
                        assert!(width(&l) <= w, "{w} columns, {p:?}: {l:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn rows_have_a_bar_from_44_columns_and_a_percent_below() {
        let r = sample();
        let wide = render(&View::Ready(&r), Period::Week, 48, 1.0);
        assert!(wide.iter().any(|l| l.contains("62%  Long context") && l.contains('█')), "{wide:#?}");
        let narrow = render(&View::Ready(&r), Period::Week, 40, 1.0);
        assert!(narrow.iter().any(|l| l.starts_with(" 62%  Long context")), "{narrow:#?}");
        assert!(!narrow.iter().any(|l| l.contains('█') || l.contains('░')));
    }

    #[test]
    fn a_row_says_one_change_under_its_name() {
        let r = sample();
        let l = render(&View::Ready(&r), Period::Week, 48, 1.0);
        let at = l.iter().position(|l| l.contains("Bash output")).unwrap();
        assert!(l[at + 1].trim() == "top: cargo test · 18 runs", "{:?}", l[at + 1]);
        assert!(l[at + 2].contains("Pipe through head or tail"));
        assert!(l.iter().any(|l| l.contains("80%  of spend on model-x  $4.10")));
        assert!(l.iter().any(|l| l.contains("55%  of spend in ~/foo  $2.20")), "{l:#?}");
        assert!(l.iter().any(|l| l.contains("22%  Curator left out 1.20M")));
    }

    #[test]
    fn the_section_has_a_selector_a_table_and_notes() {
        let r = sample();
        let l = render(&View::Ready(&r), Period::Week, 48, 1.0);
        assert!(l.iter().any(|l| l.contains("today  [7 days]  30 days") && l.ends_with("d w m")), "{l:#?}");
        assert!(l.iter().any(|l| l.starts_with("Biggest sessions") && l.ends_with("cost cache")), "{l:#?}");
        assert!(l.iter().any(|l| l.starts_with("login · today") && l.ends_with("$1.82   71%")), "{l:#?}");
        assert!(l
            .iter()
            .any(|l| l.starts_with("a-folder-name-that-is") && l.contains("… · Oct 23") && l.ends_with("n/a")));
        assert!(l.iter().any(|l| l == "Shares overlap; they do not add up to 100%."));
        assert!(l.iter().any(|l| l.contains("3 calls had no price")));
        assert!(l.iter().any(|l| l == "partial: 400 of 512 sessions"));
        assert!(l.iter().any(|l| l.contains("9 sessions · 120 calls · $5.12")));
    }

    #[test]
    fn scanning_shows_no_figures() {
        let l = render(&View::Scanning, Period::Week, 48, 1.0);
        assert_eq!(l[1], "What's contributing to your usage?");
        assert_eq!(l.last().unwrap(), "Scanning local sessions…");
        assert!(!l.iter().any(|l| l.contains('%') || l.contains('$')));
    }

    #[test]
    fn a_failed_reading_says_why() {
        let l = render(&View::Failed("cannot read /x: denied"), Period::Week, 48, 1.0);
        assert_eq!(l.last().unwrap(), "cannot read /x: denied");
    }

    #[test]
    fn an_empty_period_points_to_a_longer_one_that_has_sessions() {
        let r = sample();
        let l = render(&View::Ready(&r), Period::Day, 48, 1.0);
        assert!(l.iter().any(|l| l == "No sessions today on this machine."), "{l:#?}");
        assert!(l.iter().any(|l| l == "Try 7 days (w)."));
        let none = Ranked { by_period: Default::default(), partial: None };
        let l = render(&View::Ready(&none), Period::Week, 48, 1.0);
        assert!(l.iter().any(|l| l == "No sessions in the last 7 days on this machine."));
        assert!(!l.iter().any(|l| l.starts_with("Try")));
    }

    #[test]
    fn nothing_over_a_tenth_is_said_in_green() {
        let r = sample();
        let th = theme();
        let ls = lines(&th, &View::Ready(&r), Period::Month, 48, 1.0, None);
        let l = ls.iter().find(|l| text(l).starts_with("Nothing stands out")).expect("the line");
        assert_eq!(l.spans[0].style.fg, Some(th.success));
        assert!(!ls.iter().any(|l| text(l).contains('█')), "no rows");
    }

    #[test]
    fn colours_come_from_the_theme_by_share() {
        let r = sample();
        let th = theme();
        let ls = lines(&th, &View::Ready(&r), Period::Week, 48, 1.0, None);
        let bar_of = |name: &str| {
            let l = ls.iter().find(|l| text(l).contains(name)).unwrap();
            l.spans[0].style.fg
        };
        assert_eq!(bar_of("Long context"), Some(th.warn2), "62%");
        assert_eq!(bar_of("Bash output"), Some(th.warn), "41%");
        assert_eq!(bar_of("Cache misses"), Some(th.accent), "24%");
        assert_eq!(bar_of("Curator left out"), Some(th.success), "a win, in green");
        let track = ls.iter().find(|l| text(l).contains("Cache misses")).unwrap();
        assert_eq!(track.spans[1].style.fg, Some(th.dim2));
        let money = ls.iter().find(|l| text(l).contains("$4.10")).unwrap();
        assert_eq!(money.spans.last().unwrap().style.fg, Some(th.success));
    }

    #[test]
    fn bars_fill_in_with_the_window() {
        let r = sample();
        let at = |t: f64| {
            render(&View::Ready(&r), Period::Week, 48, t).into_iter().find(|l| l.contains("Long context")).unwrap()
        };
        assert!(at(0.0).starts_with("░░░░░░░░"), "{}", at(0.0));
        assert!(at(1.0).starts_with("█████"), "{}", at(1.0));
    }

    #[test]
    fn paths_lose_home_and_are_cut_in_the_middle() {
        assert_eq!(tilde("/home/me/foo", Some("/home/me")), "~/foo");
        assert_eq!(tilde("/home/meh/foo", Some("/home/me")), "/home/meh/foo");
        assert_eq!(tilde("/home/me", Some("/home/me")), "~");
        assert_eq!(tilde("/x", None), "/x");
        assert_eq!(cut_middle("~/code/api/server/src", 12), "~/code…r/src");
        assert_eq!(cut_middle("short", 12), "short");
        assert!(width(&cut_middle("~/日本語/日本語/日本語", 9)) <= 9);
    }

    #[test]
    fn a_scanner_reads_once_and_again_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Scanner::in_dir(Some(dir.path().to_path_buf()));
        assert!(matches!(s.view(), View::Scanning));
        s.ensure();
        assert!(s.running());
        let start = std::time::Instant::now();
        while !s.poll() && start.elapsed().as_secs() < 20 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!s.running());
        assert_eq!(s.version, 1);
        assert!(!matches!(s.view(), View::Scanning));
        s.ensure();
        assert!(!s.running(), "nothing asked for a new reading");
        s.invalidate();
        s.ensure();
        assert!(s.running());
        // a held result stays on show while the next is read
        let mut held = Scanner::with(sample());
        held.invalidate();
        held.ensure();
        assert!(matches!(held.view(), View::Ready(_)));
    }
}
