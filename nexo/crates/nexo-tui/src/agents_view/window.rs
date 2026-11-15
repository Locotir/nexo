//! The window of `/panel`: its frame, header and footer around the body.

use super::*;

impl AgentsView {
    // ---------------------------------------------------------------- drawing

    pub fn draw(&mut self, f: &mut Frame, area: Rect, theme: &Theme, now: Instant, fx: bool) {
        if area.width < 30 || area.height < 6 {
            let msg = if area.width < 30 { "make the window wider" } else { "make the window taller" };
            let msg = truncate(msg, area.width as usize);
            let y = area.y + area.height / 2;
            let line =
                Line::from(Span::styled(msg, theme.style(theme.warn))).alignment(ratatui::layout::Alignment::Center);
            f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
            self.cards.clear();
            return;
        }
        let widest = if self.zoom == Zoom::Blocks {
            MAX_BLOCK_COLS * BLOCK_W + (MAX_BLOCK_COLS - 1) * GAP
        } else {
            MAX_COLS * CARD_MAX_W + (MAX_COLS - 1) * GAP
        };
        let inner_w = area.width.min(widest + 4).saturating_sub(4);
        let zoom = self.zoom;
        let shown = self.shown();
        let groups = self.groups(&shown);
        // the cards take a sixth row for the last events, and the fleet a line of figures under the header,
        // when the terminal has the room for every session that way; else the cards keep five rows
        let pulse_ok =
            !self.policy_page && self.observing.is_none() && !self.rows.is_empty() && area.height >= PULSE_FROM;
        let layout_with = |h: u16, card_h: u16, pulse: u16| match zoom {
            Zoom::Blocks => blocks_layout(inner_w),
            Zoom::List => Layout::List,
            Zoom::Cards => {
                Self::fit_cards(&groups, cards_layout(inner_w, h), h.saturating_sub(CHROME + 1 + pulse), card_h)
            }
        };
        let (card_h, pulse) = [(CARD_H + 1, 1), (CARD_H, 1), (CARD_H, 0)]
            .into_iter()
            .filter(|(ch, pl)| (*ch == CARD_H || area.height >= RIBBON_FROM) && (*pl == 0 || pulse_ok))
            .find(|&(ch, pl)| zoom != Zoom::Cards || matches!(layout_with(area.height, ch, pl), Layout::Cards { .. }))
            .unwrap_or((CARD_H, u16::from(pulse_ok)));
        let layout_for = |h: u16| layout_with(h, card_h, pulse);
        // a blank row between the sessions and their details, when the window is tall enough to spare it
        let spare = u16::from(area.height >= SPARE_FROM);
        let want = if self.policy_page || self.observing.is_some() {
            area.height
        } else if self.view != View::Cards {
            (shown.len() as u16).saturating_add(CHROME + spare + pulse)
        } else if self.rows.is_empty() && self.filter == Filter::All {
            EMPTY_STATE.len() as u16 + CHROME + spare + pulse
        } else {
            Self::content_h(&groups, layout_for(area.height), card_h) + CHROME + spare + pulse
        };
        let h = want.clamp(8.min(area.height), area.height);
        let w = inner_w + 4;
        // the size it took when it opened: sessions coming and going never resize it
        let key = {
            use std::hash::{Hash, Hasher};
            let mut k = std::collections::hash_map::DefaultHasher::new();
            self.size_key().hash(&mut k);
            self.view.hash(&mut k);
            k.finish()
        };
        let (w, h) = self.size.keep(key, f.area(), area, || (w, h));
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        f.render_widget(Clear, r);
        let block = Box_::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.style(theme.border))
            .title(Span::styled(
                if self.policy_page {
                    " panel · policy "
                } else if self.observing.is_some() {
                    " panel · observation "
                } else if self.view == View::Map && inner_w >= map::MAP_MIN_W {
                    " panel · map "
                } else if self.view != View::Cards {
                    " panel · tree "
                } else {
                    " panel "
                },
                theme.bold(theme.accent),
            ))
            .style(Style::default().bg(theme.modal_bg));
        let inner = block.inner(r);
        f.render_widget(block, r);
        self.window = r;
        self.close = Rect::new(r.x + r.width.saturating_sub(6), r.y, 3, 1);
        let close_style =
            if self.hover { theme.bold(theme.error).add_modifier(Modifier::REVERSED) } else { theme.dim() };
        f.render_widget(Paragraph::new(Span::styled("[✗]", close_style)), self.close);

        let pad = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), inner.height);
        // the pulse row only where the window kept room for it
        let pulse = pulse.min(u16::from(r.height >= CHROME + 5));
        let rule_y = pad.y + 1 + pulse;
        // header: how many sessions are in which state, and what this view is
        f.render_widget(
            Paragraph::new(Line::from(self.header(theme, pad.width as usize, pulse == 0))),
            Rect { height: 1, ..pad },
        );
        if pulse > 0 {
            let line = self.pulse_line(theme, pad.width as usize, now, fx);
            f.render_widget(Paragraph::new(Line::from(line)), Rect { y: pad.y + 1, height: 1, ..pad });
        }
        // four rows or fewer: the rule gives way so one session line shows
        let slim = pad.height < 5 + pulse;
        if !slim {
            f.render_widget(
                Paragraph::new(Span::styled("─".repeat(pad.width as usize), theme.dim2())),
                Rect { y: rule_y, height: 1, ..pad },
            );
        }
        let foot = Rect::new(pad.x, pad.y + pad.height.saturating_sub(1), pad.width, 1);
        let detail = Rect::new(pad.x, foot.y.saturating_sub(1), pad.width, 1);
        let body_top = pad.y + if slim { 1 } else { 2 } + pulse;
        let spare = u16::from(r.height >= SPARE_FROM);
        let body = Rect::new(pad.x, body_top, pad.width, detail.y.saturating_sub(body_top + spare));
        self.cards.clear();
        if self.policy_page {
            self.draw_policy(f, body, theme);
        } else if self.observing.is_some() {
            self.draw_observation(f, body, theme);
        } else {
            self.glide.note(Some(self.selected), now, fx);
            self.note(now, fx);
            let (layout, card_h) = match self.zoom {
                Zoom::Blocks if body.height >= BLOCK_H => (blocks_layout(body.width), card_h),
                Zoom::Blocks | Zoom::List => (Layout::List, card_h),
                Zoom::Cards => {
                    // a window that kept an older size may have lost the room for the sixth row
                    let at = |ch: u16| Self::fit_cards(&groups, cards_layout(body.width, body.height), body.height, ch);
                    match at(card_h) {
                        Layout::List if card_h > CARD_H => (at(CARD_H), CARD_H),
                        other => (other, card_h),
                    }
                }
            };
            match self.view {
                View::Map if body.width >= map::MAP_MIN_W => self.draw_map(f, body, &shown, theme),
                View::Tree | View::Map => self.draw_tree(f, body, &shown, theme),
                View::Cards => self.draw_bands(f, body, layout, card_h, &shown, &groups, theme, now, fx),
            }
            let lane = Rect { y: rule_y, height: 1, ..pad };
            if !slim {
                self.draw_flights(f, lane, body, &shown, theme, now);
                self.draw_letter(f, lane, body, &shown, theme, now);
            }
            if let Some(row) = shown.get(self.selected) {
                f.render_widget(Paragraph::new(Line::from(self.detail(row, theme, detail.width as usize))), detail);
            }
        }
        let more = self.more_marks();
        let more = more.as_str();
        let line = if self.policy_page {
            Line::from(Span::styled(truncate("↑↓ scroll · o/Esc back", foot.width as usize), theme.dim2()))
        } else if let Some(t) = &self.edit {
            // the end of the text is what is being typed: it is the part kept when the line is long
            let gone = self.edit_for.as_ref().is_some_and(|(k, _)| !self.rows.iter().any(|r| &r.key == k));
            let tails: [&str; 3] = if gone {
                ["  session gone · Esc keep", "  gone · Esc", ""]
            } else {
                ["  Enter send · Esc cancel", "  Enter · Esc", ""]
            };
            let w = foot.width as usize;
            // the longest tail that still leaves 12 columns for the message
            let tail = tails.into_iter().find(|s| w >= width(s) + 12).unwrap_or("");
            let shown =
                tail_of(&format!("message › {t}▏"), w.saturating_sub(width(tail)), "message › ".chars().count());
            Line::from(vec![
                Span::styled(shown, theme.style(theme.accent)),
                Span::styled(tail, if gone { theme.style(theme.warn) } else { theme.dim2() }),
            ])
        } else if let Some(n) = &self.note {
            Line::from(Span::styled(truncate(n, foot.width as usize), theme.style(theme.accent2)))
        } else if self.observing.is_some() {
            Line::from(Span::styled(truncate("↑↓ scroll · n message · Esc back", foot.width as usize), theme.dim2()))
        } else {
            // "more below" is the part that must survive a narrow window
            let room = (foot.width as usize).saturating_sub(width(more));
            let hints: &[&str] = if self.panel.is_some() {
                &[
                    "←↑↓→ move · v view · s source · Enter observe · n message · o policy · p/P pause · +/- zoom · Esc close",
                    "←↑↓→ move · s source · n message · o policy · p pause · +/- zoom · Esc close",
                    "s source · n message · p pause · Esc close",
                    "v view · s source · Esc close",
                    "s source · Esc close",
                    "Esc close",
                ]
            } else {
                &[
                    "←↑↓→ move · v view · Enter observe · s source · +/- zoom · Esc close",
                    "←↑↓→ move · s source · +/- zoom · Esc close",
                    "v view · s source · Esc close",
                    "s source · Esc close",
                    "Esc close",
                ]
            };
            let hint = hints.iter().find(|h| width(h) <= room).copied().unwrap_or("Esc close");
            Line::from(vec![
                Span::styled(truncate(hint, room), theme.dim2()),
                Span::styled(more, theme.style(theme.accent2)),
            ])
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    /// `↑ 2 ↓ 3` when sessions sit out of sight above and below, so a scrolled list never looks complete.
    fn more_marks(&self) -> String {
        let total = self.shown().len();
        let Some((first, last)) = self
            .cards
            .iter()
            .map(|(i, _)| *i)
            .fold(None, |m: Option<(usize, usize)>, i| Some(m.map_or((i, i), |(a, b)| (a.min(i), b.max(i)))))
        else {
            return String::new();
        };
        let below = total.saturating_sub(last + 1);
        match (first, below) {
            (0, 0) => String::new(),
            (0, n) => format!("  ↓ {n} more"),
            (n, 0) => format!("  ↑ {n} above"),
            (a, b) => format!("  ↑ {a} ↓ {b}"),
        }
    }

    fn header(&self, theme: &Theme, w: usize, fold: bool) -> Vec<Span<'static>> {
        let order = [
            State::WaitingPermission,
            State::WaitingAnswer,
            State::Stalled,
            State::Looping,
            State::Working,
            State::Idle { min: 0 },
            State::Done,
        ];
        let shown = self.shown();
        let counts: Vec<(State, usize)> = order
            .into_iter()
            .map(|s| {
                (s, shown.iter().filter(|r| std::mem::discriminant(&r.state) == std::mem::discriminant(&s)).count())
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        // the words when they fit, else the glyph and the number alone: a count is never left out
        let build = |full: bool| -> (Vec<Span<'static>>, usize) {
            let mut out: Vec<Span<'static>> = Vec::new();
            let mut used = 0;
            for (s, n) in &counts {
                let (g, label) = chip(*s);
                let text = if full { format!("{g} {n} {label}") } else { format!("{g}{n}") };
                if used > 0 {
                    out.push(Span::raw(if full { "  " } else { " " }));
                    used += if full { 2 } else { 1 };
                }
                used += width(&text);
                out.push(Span::styled(text, theme.style(chip_color(theme, *s))));
            }
            if out.is_empty() {
                out.push(Span::styled("no sessions", theme.dim()));
                used = 11;
            }
            (out, used)
        };
        let (mut left, mut used) = build(true);
        if used > w {
            (left, used) = build(false);
        }
        let paused_all = self.panel.as_ref().is_some_and(|p| p.paused_all());
        let rights: [&str; 2] = match (&self.panel, paused_all) {
            (None, _) => ["watching · read-only", "read-only"],
            (Some(_), true) => ["supervisor paused", "paused"],
            (Some(_), false) => ["supervising · rules", "supervising"],
        };
        // which sessions this is, ahead of what the supervisor does; "+N" counts those the filter hides
        let hidden = self.rows.len() - shown.len();
        let which =
            if hidden > 0 { format!("{} +{hidden}", self.filter.label()) } else { self.filter.label().to_string() };
        // with no pulse row of its own, the fleet's figures ride at the front of the right side
        let fold = if fold { self.pulse_fold(&shown) } else { None };
        let mut candidates: Vec<String> = Vec::new();
        if let Some(p) = &fold {
            candidates.push(format!("{p} · {which} · {}", rights[0]));
            candidates.push(format!("{p} · {which} · {}", rights[1]));
            candidates.push(format!("{p} · {which}"));
        }
        candidates.extend([
            format!("{which} · {}", rights[0]),
            format!("{which} · {}", rights[1]),
            which.clone(),
            which.split(' ').next().unwrap_or("").to_string(),
        ]);
        for right in &candidates {
            let rw = width(right);
            if used + rw + 2 <= w {
                left.push(Span::raw(" ".repeat(w - used - rw)));
                left.push(Span::styled(right.clone(), theme.dim2()));
                break;
            }
        }
        left
    }
}

/// The last `room` columns of `text`, with an ellipsis in front when something was cut; the first `keep`
/// characters (a label) are always kept.
pub(super) fn tail_of(text: &str, room: usize, keep: usize) -> String {
    if width(text) <= room {
        return text.to_string();
    }
    let head: String = text.chars().take(keep).collect();
    let rest: Vec<char> = text.chars().skip(keep).collect();
    let mut budget = room.saturating_sub(width(&head) + 1);
    let mut tail: Vec<char> = Vec::new();
    for c in rest.iter().rev() {
        let w = width(&c.to_string());
        if w > budget {
            break;
        }
        budget -= w;
        tail.push(*c);
    }
    tail.reverse();
    format!("{head}…{}", tail.into_iter().collect::<String>())
}
