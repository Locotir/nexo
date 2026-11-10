//! `/sent` (`/request`): the last request exactly as it was sent, in a scrollable window. The system prompt
//! with the parts it is made of, the tools offered, then every message part by part, each with its size in
//! tokens. What the context curator or the supervisor put in carries a tag and a colour. A tool result shows
//! its first lines until it is opened. `/` searches, `n` and `N` go to the next and the previous match, `y`
//! copies the section at the top, `e` opens or folds it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexo_core::agent::SentSection;
use ratatui::text::{Line, Span};

use crate::modal::ModalOut;
use crate::render::blocks::fmt_k;
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::theme::Theme;

/// Lines of a folded section shown before `… N more lines`.
const FOLDED_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    /// The first match at or under the top of the view (a search just made).
    First,
    Next,
    Prev,
}

#[derive(Debug, Clone)]
pub struct SentView {
    model: String,
    sections: Vec<SentSection>,
    /// Which folded sections the person opened.
    open: Vec<bool>,
    pub scroll: usize,
    /// The line each section starts at, as last laid out.
    starts: Vec<usize>,
    /// The search in force, and the one being typed.
    query: String,
    typing: Option<String>,
    /// The lines that match, as last laid out, and the one the view went to.
    matches: Vec<usize>,
    at: usize,
    jump: Option<Jump>,
}

impl SentView {
    pub fn new(model: String, sections: Vec<SentSection>) -> SentView {
        let open = vec![false; sections.len()];
        SentView {
            model,
            sections,
            open,
            scroll: 0,
            starts: Vec::new(),
            query: String::new(),
            typing: None,
            matches: Vec::new(),
            at: 0,
            jump: None,
        }
    }

    /// About how many tokens the request takes.
    pub fn tokens(&self) -> u64 {
        self.sections.iter().filter(|s| s.depth == 0).map(|s| s.tokens).sum()
    }

    pub fn title(&self) -> String {
        format!(" request · {} · ~{} tokens ", self.model, fmt_k(self.tokens()))
    }

    /// The section the first line of the view belongs to.
    fn current(&self) -> Option<usize> {
        self.starts.partition_point(|s| *s <= self.scroll).checked_sub(1)
    }

    /// What `y` copies for section `i`: its text; for the head of the system prompt, all its parts.
    fn copy_text(&self, i: usize) -> String {
        let s = &self.sections[i];
        if s.depth == 0 && s.text.is_empty() {
            let parts: Vec<&str> =
                self.sections[i + 1..].iter().take_while(|x| x.depth > 0).map(|x| x.text.as_str()).collect();
            if !parts.is_empty() {
                return parts.join("\n\n");
            }
        }
        s.text.clone()
    }

    /// Whether section `i` shows only its first lines.
    fn folded(&self, i: usize, q: &str) -> bool {
        let s = &self.sections[i];
        // a search opens what holds the words
        s.collapse
            && !self.open[i]
            && s.text.lines().count() > FOLDED_LINES
            && (q.is_empty() || !s.text.to_lowercase().contains(q))
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(t) = &mut self.typing {
            match key.code {
                KeyCode::Esc => self.typing = None,
                KeyCode::Enter => {
                    self.query = t.trim().to_string();
                    self.typing = None;
                    self.jump = Some(Jump::First);
                }
                KeyCode::Backspace => {
                    t.pop();
                }
                KeyCode::Char('u') if ctrl => t.clear(),
                KeyCode::Char(c) if !ctrl => t.push(c),
                _ => {}
            }
            return ModalOut::Nothing;
        }
        match key.code {
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();
                self.matches.clear();
            }
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Char('/') => self.typing = Some(String::new()),
            KeyCode::Char('n') | KeyCode::Enter if !self.query.is_empty() => self.jump = Some(Jump::Next),
            KeyCode::Char('N') if !self.query.is_empty() => self.jump = Some(Jump::Prev),
            KeyCode::Char('y') => {
                if let Some(i) = self.current() {
                    return ModalOut::Copy(self.copy_text(i));
                }
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if let Some(i) = self.current().filter(|i| self.sections[*i].collapse) {
                    self.open[i] = !self.open[i];
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = usize::MAX / 2,
            _ => {}
        }
        ModalOut::Nothing
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }

    /// The rows pinned under the body: a blank line, then the search being typed or the keys.
    pub fn pinned(&self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        if let Some(t) = &self.typing {
            return vec![
                Line::default(),
                Line::from(vec![
                    Span::styled("/ ", theme.style(theme.accent)),
                    Span::raw(truncate(t, w.saturating_sub(30))),
                    Span::styled("▏", theme.dim()),
                    Span::styled("  Enter searches · Esc cancels", theme.dim()),
                ]),
            ];
        }
        let copy = match self.current() {
            Some(i) => format!("copy {}", truncate(&self.sections[i].heading, 28)),
            None => "copy".into(),
        };
        let mut sets: Vec<Vec<(String, String)>> = Vec::new();
        let kv = |a: &str, b: &str| (a.to_string(), b.to_string());
        if self.query.is_empty() {
            sets.push(vec![
                kv("↑↓", "scroll"),
                kv("/", "search"),
                kv("y", &copy),
                kv("e", "open or fold"),
                kv("Esc", "close"),
            ]);
            sets.push(vec![kv("/", "search"), kv("y", "copy"), kv("Esc", "close")]);
        } else {
            let state = if self.matches.is_empty() {
                format!("\"{}\" · no match", self.query)
            } else {
                format!("\"{}\" · {}/{}", self.query, self.at.min(self.matches.len() - 1) + 1, self.matches.len())
            };
            sets.push(vec![kv("", &state), kv("n N", "next, previous"), kv("y", &copy), kv("Esc", "clear")]);
            sets.push(vec![kv("", &state), kv("n N", "move"), kv("Esc", "clear")]);
        }
        sets.push(vec![kv("Esc", "closes")]);
        for set in &sets {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                if !k.is_empty() {
                    spans.push(Span::styled(k.clone(), theme.style(theme.accent)));
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(what.clone(), theme.dim()));
            }
            if spans.iter().map(|s| width(&s.content)).sum::<usize>() <= w {
                return vec![Line::default(), Line::from(spans)];
            }
        }
        vec![Line::default(), Line::from(Span::styled("Esc close", theme.dim()))]
    }

    /// The body at `w` columns. Also records where each section starts and which lines match the search,
    /// and carries out the jump a key asked for.
    pub fn layout(&mut self, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        let q = self.query.to_lowercase();
        let w = w.max(8);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut starts = Vec::with_capacity(self.sections.len());
        for (i, s) in self.sections.iter().enumerate() {
            if s.depth == 0 && !lines.is_empty() {
                lines.push(Line::default());
            }
            starts.push(lines.len());
            let tone = match s.tag.as_deref() {
                Some("curator") => Some(theme.accent2),
                Some("supervisor") => Some(theme.warn),
                Some(_) => Some(theme.accent),
                None => None,
            };
            let mut head: Vec<Span<'static>> = Vec::new();
            if s.depth == 0 {
                head.push(Span::styled("▌ ", tone.map_or(theme.dim2(), |c| theme.style(c))));
                head.push(Span::styled(s.heading.clone(), theme.bold(theme.fg)));
            } else {
                head.push(Span::styled("  ▸ ", theme.dim2()));
                head.push(Span::styled(s.heading.clone(), theme.bold(tone.unwrap_or(theme.dim))));
            }
            head.push(Span::styled(format!(" · {} tokens", fmt_k(s.tokens)), theme.dim()));
            if let (Some(tag), Some(c)) = (&s.tag, tone) {
                head.push(Span::styled(format!("  [{tag}]"), theme.bold(c)));
            }
            lines.push(Line::from(head));
            if s.text.is_empty() {
                continue;
            }
            let (indent, style) = match tone {
                Some(c) => (if s.depth == 0 { "▎ " } else { "  ▎ " }, theme.style(c)),
                None => (if s.depth == 0 { "  " } else { "    " }, Default::default()),
            };
            let prefix = [Span::styled(indent, tone.map_or(theme.dim2(), |c| theme.style(c)))];
            let all: Vec<&str> = s.text.lines().collect();
            if self.folded(i, &q) {
                lines.extend(wrap_plain(&all[..FOLDED_LINES].join("\n"), w, style, &prefix));
                let more = format!("… {} more lines · e opens", all.len() - FOLDED_LINES);
                lines.push(Line::from(vec![Span::raw(indent.replace('▎', " ")), Span::styled(more, theme.dim2())]));
            } else {
                lines.extend(wrap_plain(&s.text, w, style, &prefix));
            }
        }
        self.starts = starts;
        // the lines with the words in them
        self.matches = if q.is_empty() {
            Vec::new()
        } else {
            let text =
                |l: &Line<'static>| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().to_lowercase();
            lines.iter().enumerate().filter(|(_, l)| text(l).contains(&q)).map(|(i, _)| i).collect()
        };
        if let Some(j) = self.jump.take() {
            let n = self.matches.len();
            if n > 0 {
                self.at = match j {
                    Jump::First => self.matches.partition_point(|m| *m < self.scroll) % n,
                    Jump::Next => (self.at + 1) % n,
                    Jump::Prev => (self.at + n - 1) % n,
                };
                self.scroll = self.matches[self.at];
            }
        }
        for (k, &m) in self.matches.iter().enumerate() {
            let hl = if k == self.at { theme.selection() } else { theme.hover() };
            for s in &mut lines[m].spans {
                s.style = s.style.patch(hl);
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn sec(heading: &str, depth: u8, tag: Option<&str>, text: &str, collapse: bool) -> SentSection {
        SentSection {
            heading: heading.into(),
            depth,
            tag: tag.map(str::to_string),
            tokens: (text.len() as u64).div_ceil(4),
            text: text.into(),
            collapse,
        }
    }

    fn view() -> SentView {
        SentView::new(
            "m1".into(),
            vec![
                sec("System prompt", 0, None, "", false),
                sec("built-in text", 1, None, "You are nexo.", false),
                sec("curator directives", 1, Some("curator"), "[curator] Working efficiently", false),
                sec("#1 user", 0, None, "fix the bug", false),
                sec("#1 user · standing rules", 0, Some("curator"), "[curator] Standing instructions: tabs", false),
                sec("#2 assistant · call read", 0, None, "{\"path\":\"a.rs\"}", false),
                sec("#3 user · result of read", 0, None, "l1\nl2\nl3\nl4\nl5\nneedle here", true),
            ],
        )
    }

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    fn press(v: &mut SentView, k: KeyCode) -> ModalOut {
        v.key(KeyEvent::new(k, KeyModifiers::NONE))
    }

    #[test]
    fn it_lays_out_each_section_with_its_size_and_the_curators_tag() {
        for theme in [Theme::dark(), Theme::light()] {
            let mut v = view();
            let t = plain(&v.layout(80, &theme));
            assert!(t[0].starts_with("▌ System prompt · "), "{t:?}");
            assert!(t.iter().any(|l| l.contains("curator directives") && l.contains("[curator]")), "{t:?}");
            assert!(
                t.iter().any(|l| l.contains("#1 user · standing rules") && l.contains("tokens  [curator]")),
                "{t:?}"
            );
            assert!(
                t.iter().any(|l| l.contains("▎ [curator] Standing instructions: tabs")),
                "the curator's text is marked: {t:?}"
            );
            assert!(t.iter().any(|l| l == "  fix the bug"), "{t:?}");
            // a tool result shows its first lines
            assert!(t.iter().any(|l| l.contains("l3")) && !t.iter().any(|l| l.contains("l4")), "{t:?}");
            assert!(t.iter().any(|l| l.contains("… 3 more lines · e opens")), "{t:?}");
            assert_eq!(v.starts.len(), 7);
            assert!(v.title().contains("m1") && v.title().contains("tokens"));
        }
    }

    #[test]
    fn e_opens_the_section_at_the_top_and_y_copies_it() {
        let theme = Theme::dark();
        let mut v = view();
        v.layout(80, &theme);
        v.scroll = v.starts[6];
        assert_eq!(press(&mut v, KeyCode::Char('e')), ModalOut::Nothing);
        let t = plain(&v.layout(80, &theme));
        assert!(t.iter().any(|l| l.contains("needle here")) && !t.iter().any(|l| l.contains("more lines")), "{t:?}");
        assert_eq!(
            press(&mut v, KeyCode::Char('y')),
            ModalOut::Copy("l1\nl2\nl3\nl4\nl5\nneedle here".into()),
            "what is copied is the whole text, opened or not"
        );
        // the head of the system prompt copies all its parts
        v.scroll = 0;
        assert_eq!(
            press(&mut v, KeyCode::Char('y')),
            ModalOut::Copy("You are nexo.\n\n[curator] Working efficiently".into())
        );
        assert_eq!(press(&mut v, KeyCode::Esc), ModalOut::Close);
    }

    #[test]
    fn a_search_opens_what_holds_the_words_and_n_goes_through_the_matches() {
        let theme = Theme::dark();
        let mut v = view();
        v.layout(80, &theme);
        assert_eq!(press(&mut v, KeyCode::Char('/')), ModalOut::Nothing);
        for c in "NEEDLE".chars() {
            press(&mut v, KeyCode::Char(c));
        }
        let typing = plain(&v.pinned(80, &theme));
        assert!(typing[1].starts_with("/ NEEDLE"), "{typing:?}");
        press(&mut v, KeyCode::Enter);
        let t = plain(&v.layout(80, &theme));
        let at = t.iter().position(|l| l.contains("needle here")).expect("opened by the search");
        assert_eq!(v.scroll, at, "the view went to the match");
        assert_eq!(v.matches, [at]);
        let keys = plain(&v.pinned(100, &theme));
        assert!(keys[1].contains("\"NEEDLE\" · 1/1"), "{keys:?}");
        // the match is lit
        let lit =
            v.layout(80, &theme)[at].spans.iter().any(|s| s.style.bg.is_some() && s.style.bg != Some(Color::Reset));
        assert!(lit);
        // a search with no match says so, and Esc clears it before it closes
        press(&mut v, KeyCode::Char('/'));
        for c in "zzz".chars() {
            press(&mut v, KeyCode::Char(c));
        }
        press(&mut v, KeyCode::Enter);
        v.layout(80, &theme);
        assert!(plain(&v.pinned(100, &theme))[1].contains("no match"));
        assert_eq!(press(&mut v, KeyCode::Esc), ModalOut::Nothing);
        assert_eq!(press(&mut v, KeyCode::Esc), ModalOut::Close);
    }

    #[test]
    fn the_keys_scroll_and_the_window_follows_a_long_text() {
        let theme = Theme::dark();
        let mut v = SentView::new("m".into(), vec![sec("#1 user", 0, None, &"word ".repeat(400), false)]);
        let t = v.layout(40, &theme);
        assert!(t.len() > 10, "a long line wraps");
        press(&mut v, KeyCode::Down);
        assert_eq!(v.scroll, 1);
        press(&mut v, KeyCode::PageDown);
        assert_eq!(v.scroll, 11);
        press(&mut v, KeyCode::Home);
        assert_eq!(v.scroll, 0);
        v.scroll_by(-5);
        assert_eq!(v.scroll, 0);
    }
}
