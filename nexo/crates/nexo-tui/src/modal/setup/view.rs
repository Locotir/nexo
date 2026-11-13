//! What the setup draws: title, hints and the body lines, and the rows under the mouse.

use super::*;

impl Setup {
    pub fn title(&self) -> String {
        match self.step.name() {
            "" => " set up a model ".to_string(),
            n => format!(" set up a model · {n} "),
        }
    }

    /// The row of the text being typed, if the step has one: it stays in view when the body scrolls.
    pub fn prompt_row(lines: &[Line<'_>]) -> Option<usize> {
        lines.iter().position(|l| l.spans.first().is_some_and(|s| s.content == "› "))
    }

    /// The row of the field being edited at the review step (the one drawn bold, after a space).
    pub fn selected_row(lines: &[Line<'_>]) -> Option<usize> {
        lines.iter().position(|l| {
            l.spans.first().is_some_and(|s| s.content.starts_with(' ') && s.style.add_modifier.contains(Modifier::BOLD))
        })
    }

    pub fn hints(&self) -> &'static str {
        if self.busy.is_some() && self.step != Step::Login {
            return " Esc cancel";
        }
        match self.step {
            Step::Start => " ↑↓ choose · Enter pick · paste a key · Esc close",
            Step::Provider if self.from_start => " type to search · ↑↓ choose · Enter pick · Esc back to the choices",
            Step::Provider => " type to filter · ↑↓ choose · ←→ fold · Enter open or pick · Esc close",
            Step::Login if self.busy.is_some() => " finish in the browser · Esc cancel",
            Step::Login => " ↑↓ choose · Enter sign in · Esc back",
            Step::Model => " type to filter (or a name not listed) · ↑↓ choose · Enter test it · Esc back",
            Step::Address => " type · Enter check · Ctrl+U clear · Esc back",
            Step::Key if keys::page(&self.name).is_some() => {
                " paste the key · Enter test it · Ctrl+O open the page · Esc back"
            }
            Step::Key => " paste the key · Enter test it · Esc back",
            Step::Review if !self.details => " Enter save and use · Tab change details · Esc back",
            Step::Review => " ↑↓ field · type to change · ←→ effort · Enter save and use · Esc back",
            Step::Trouble => match self.again {
                Again::Key => " Enter paste the key again · Tab details · Esc pick another provider",
                Again::Address => " Enter edit the address · Tab details · Esc pick another provider",
                Again::Probe => " Enter try again · Tab details · Esc pick another provider",
                Again::Check => " Enter try again · S save anyway · Tab details · Esc pick another model",
            },
            Step::Done => " Enter or Esc to start",
        }
    }

    /// The body: the lines above the list, and the list itself for the steps that have one.
    /// Returns the lines and the index of the first list row.
    pub fn lines(&self, w: usize, theme: &Theme, spinner: usize, short: bool) -> (Vec<Line<'static>>, Option<usize>) {
        let dim = theme.dim();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let prompt = |text: String, masked: bool| {
            let shown = if masked { keys::mask(&text) } else { text };
            Line::from(vec![
                Span::styled("› ", theme.style(theme.accent)),
                Span::raw(shown),
                Span::styled("▏", theme.dim()),
            ])
        };
        let say = |lines: &mut Vec<Line<'static>>, text: &str, style: Style| {
            lines.extend(wrap_plain(text, w, style, &[]));
        };
        match self.step {
            Step::Start => {
                lines.push(self.head_line(
                    "nexo needs a model to talk to. Choose where to get one.",
                    w,
                    theme,
                    spinner,
                ));
                if !self.scanned && self.busy.is_none() && self.error.is_none() {
                    say(&mut lines, "Looking for local models…", dim);
                }
                if !self.field.is_empty() {
                    lines.push(prompt(self.field.clone(), true));
                }
            }
            // on a short screen the list comes first: the intro is dropped
            Step::Provider if short && self.busy.is_none() && self.error.is_none() => {}
            Step::Provider => {
                let ask = match (self.pending_key.is_some(), self.from_start) {
                    (true, _) => "Which service is this key from? Type to search.",
                    (false, true) => "Choose a provider. Type to search.",
                    (false, false) => "Pick a model, or set up a provider. Type to search.",
                };
                lines.push(self.head_line(ask, w, theme, spinner));
                if !self.scanned && !self.from_start && self.busy.is_none() && self.error.is_none() {
                    say(&mut lines, "Looking for local models…", dim);
                }
            }
            Step::Login => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled("sign in with your subscription", dim),
                ]));
                if let Some(cost) = self.cost_line() {
                    say(&mut lines, cost, dim);
                }
                if let Some((url, code)) = &self.prompt {
                    lines.push(Line::default());
                    // the host only; the whole link is long, and `c` copies it
                    lines.push(Line::from(vec![
                        Span::styled("opened ", dim),
                        Span::styled(host_of(url).to_string(), theme.style(theme.accent)),
                        Span::styled(" in your browser · c copies the link", dim),
                    ]));
                    if !code.is_empty() {
                        let spaced = code.chars().map(String::from).collect::<Vec<_>>().join(" ");
                        lines.push(Line::default());
                        lines.push(Line::from(vec![
                            Span::styled("type this code  ", dim),
                            Span::styled(spaced, theme.bold(theme.accent)),
                        ]));
                    }
                }
            }
            Step::Address => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{} ", self.name), theme.bold(theme.accent)),
                    Span::styled("address of the server", dim),
                ]));
                if let Some(s) = &self.signed {
                    lines.push(Line::from(Span::styled(format!("✓ {s}"), theme.style(theme.success))));
                }
                // the reason it failed takes the place of the example, so the dialog does not grow
                match &self.error {
                    Some(e) => lines.push(Line::from(Span::styled(
                        truncate(&format!("✗ {e}"), w.max(8)),
                        theme.style(theme.error),
                    ))),
                    None => say(&mut lines, "e.g. localhost:11434 (Ollama) or localhost:8000/v1 (vLLM)", dim),
                }
                lines.push(prompt(self.field.clone(), false));
            }
            Step::Key => lines.extend(self.key_lines(w, theme)),
            Step::Trouble => lines.extend(self.trouble_lines(w, theme)),
            Step::Done => lines.extend(self.done_lines(w, theme)),
            Step::Model => {
                if let Some(s) = &self.signed {
                    lines.push(Line::from(Span::styled(format!("✓ {s}"), theme.style(theme.success))));
                }
                if let Some(p) = &self.probe {
                    let n = p.models.len();
                    let on_this_computer = LOCAL.iter().any(|(l, _)| *l == self.name);
                    let mut head = if on_this_computer {
                        let mut h = format!("✓ {}", kind_label(&p.kind));
                        if let Some(v) = &p.version {
                            h.push_str(&format!(" {v}"));
                        }
                        h.push_str(" on this computer");
                        h
                    } else if self.name == "custom" {
                        format!("✓ {} at {}", kind_label(&p.kind), p.base_url)
                    } else if self.has_key() && self.cfg.login.is_none() && p.key_proved {
                        "✓ The key works".to_string()
                    } else if self.has_key() && self.cfg.login.is_none() {
                        // a list anyone can read says nothing of the key: the test request will
                        format!("✓ Models found at {}", self.name)
                    } else {
                        format!("✓ Connected to {}", self.name)
                    };
                    head.push_str(&match n {
                        0 => String::new(),
                        1 => " · 1 model".to_string(),
                        n => format!(" · {n} models"),
                    });
                    lines.push(Line::from(Span::styled(truncate(&head, w.max(8)), theme.style(theme.success))));
                    if n == 0 {
                        lines.extend(self.empty_note(w, theme));
                    } else if let Some(about) = self.model_line() {
                        say(&mut lines, &about, dim);
                    }
                }
                lines.push(prompt(self.list.query.clone(), looks_like_secret(&self.list.query)));
            }
            Step::Review if !self.details => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{}/{}", self.name, self.model),
                    theme.bold(theme.accent),
                )]));
                if let Some(c) = &self.check {
                    let ms = c.latency_ms.map(crate::render::blocks::fmt_ms).unwrap_or_default();
                    lines.push(Line::from(Span::styled(
                        format!("✓ Works: {} answered in {ms}", self.model),
                        theme.style(theme.success),
                    )));
                    if !c.called_tool {
                        say(
                            &mut lines,
                            "It did not use the test tool, so tools may not work.",
                            theme.style(theme.warn),
                        );
                    }
                }
            }
            Step::Review => {
                lines.push(Line::from(vec![Span::styled(
                    format!("{}/{}", self.name, self.model),
                    theme.bold(theme.accent),
                )]));
                if let Some(c) = &self.check {
                    match (&c.error, c.latency_ms) {
                        (Some(e), _) => {
                            say(&mut lines, &format!("✗ the test request failed: {e}"), theme.style(theme.error))
                        }
                        (None, Some(ms)) => {
                            let mut s = format!("✓ answered in {}", crate::render::blocks::fmt_ms(ms));
                            s.push_str(if c.called_tool {
                                " · calls tools ✓"
                            } else {
                                " · did not call the test tool: tools may not work"
                            });
                            if c.reasoned {
                                s.push_str(" · reasons");
                            }
                            let style =
                                if c.called_tool { theme.style(theme.success) } else { theme.style(theme.warn) };
                            say(&mut lines, &s, style);
                        }
                        _ => {}
                    }
                    for s in &c.sources {
                        say(&mut lines, &format!("  {s}"), dim);
                    }
                }
                lines.push(Line::default());
                for (i, f) in self.fields.iter().enumerate() {
                    lines.push(self.row(i, f.label, &f.value, &f.source, w, theme));
                }
                if !self.efforts.is_empty() {
                    let v = match self.effort {
                        Some(i) => format!("‹ {} ›", self.efforts[i]),
                        None => "‹ default ›".to_string(),
                    };
                    let src = format!("takes {}", self.efforts.join(", "));
                    lines.push(self.row(self.fields.len(), "effort", &v, &src, w, theme));
                }
            }
        }
        let in_head = matches!(self.step, Step::Start | Step::Provider);
        if let (Some(b), false) = (&self.busy, in_head) {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                format!("{} {b}…", SPINNER[spinner % SPINNER.len()]),
                theme.style(theme.accent),
            )));
        }
        let own_line = in_head || matches!(self.step, Step::Address | Step::Key);
        if let (Some(e), false) = (&self.error, own_line) {
            lines.push(Line::default());
            if self.step == Step::Login {
                // the sign-in failed: one line for the reason, then what the keys do
                let one = e.lines().next().unwrap_or("");
                lines.push(Line::from(Span::styled(truncate(&format!("✗ {one}"), w.max(8)), theme.style(theme.error))));
                lines.push(Line::from(Span::styled("Enter retry · Esc back", dim)));
            } else {
                say(&mut lines, &format!("✗ {e}"), theme.style(theme.error));
            }
        }
        // a server with no models yet has said so above: no "no match" under an empty prompt, no list to follow
        let nothing_listed = self.step == Step::Model && self.list.items.is_empty() && self.list.query.is_empty();
        let list_at = (matches!(self.step, Step::Start | Step::Provider | Step::Model | Step::Login)
            && !nothing_listed)
            .then(|| {
                lines.push(Line::default());
                lines.len()
            });
        if list_at.is_some() {
            if self.list.ranked.is_empty() {
                let none = if self.step == Step::Model && !self.list.query.trim().is_empty() {
                    "no match · Enter try it as typed"
                } else {
                    "no match"
                };
                lines.push(Line::from(Span::styled(none, dim)));
            }
            for (i, &idx) in self.list.ranked.iter().enumerate() {
                let it = &self.list.items[idx];
                let label = truncate(&it.label, w.saturating_sub(2));
                let style = if i == self.list.selected {
                    theme.selection().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                // a header and the row of one's own server: the name, then its facts after it
                if self.step == Step::Provider && (it.value.starts_with("group:") || it.value == CUSTOM) {
                    let used = crate::render::wrap::width(&label) + 5;
                    let hint = truncate(&it.hint, w.saturating_sub(used));
                    let pad = w.saturating_sub(used + crate::render::wrap::width(&hint));
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {label}"), style.patch(theme.bold(theme.accent)).patch(style)),
                        Span::styled(format!("  · {hint}"), style.patch(theme.dim())),
                        Span::styled(" ".repeat(pad), style),
                    ]));
                    continue;
                }
                let hint = truncate(&it.hint, w.saturating_sub(crate::render::wrap::width(&label) + 4));
                let pad = w.saturating_sub(crate::render::wrap::width(&label) + crate::render::wrap::width(&hint) + 2);
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label}"), style),
                    Span::styled(" ".repeat(pad), style),
                    Span::styled(hint, style.patch(theme.dim())),
                    Span::styled(" ", style),
                ]));
            }
        }
        (lines, list_at)
    }

    pub(super) fn row(
        &self,
        i: usize,
        label: &str,
        value: &str,
        source: &str,
        w: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let sel = i == self.selected;
        let style = if sel { theme.selection().add_modifier(Modifier::BOLD) } else { Style::default() };
        let cursor = if sel { "▏" } else { "" };
        let left = format!(" {label:<15}{value}{cursor}");
        let src = truncate(source, w.saturating_sub(crate::render::wrap::width(&left) + 3));
        let pad = w.saturating_sub(crate::render::wrap::width(&left) + crate::render::wrap::width(&src) + 1);
        Line::from(vec![
            Span::styled(left, style),
            Span::styled(" ".repeat(pad), style),
            Span::styled(src, style.patch(theme.dim())),
            Span::styled(" ", style),
        ])
    }

    /// The list row under a click, when the list is shown.
    pub fn click_row(&mut self, i: usize) -> SetupOut {
        if matches!(self.step, Step::Start | Step::Provider | Step::Model | Step::Login) && i < self.list.ranked.len() {
            self.list.selected = i;
            return self.pick();
        }
        SetupOut::Nothing
    }

    pub fn scroll_by(&mut self, d: i32) {
        match self.step {
            Step::Start | Step::Provider | Step::Model | Step::Login => self.move_list(d),
            Step::Review if self.details => {
                let rows = (self.fields.len() + usize::from(!self.efforts.is_empty())) as i32;
                if rows > 0 {
                    self.selected = (self.selected as i32 + d).rem_euclid(rows) as usize;
                }
            }
            _ => {}
        }
    }
}
