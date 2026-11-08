//! Mouse and selection, scrollbars, hover and the editor's own pointer handling.

use super::*;

impl App {
    /// `/context-live`: the context map in the side panel.
    pub fn toggle_live(&mut self, term_width: u16) {
        if !self.show_live && term_width < FILES_PANEL_MIN_W {
            self.flash_level(Level::Warn, &format!("the context map needs {FILES_PANEL_MIN_W} columns"));
            return;
        }
        self.show_live = !self.show_live;
        if self.show_live && self.map.is_empty() {
            self.flash("the map fills in with the next model call");
        }
        self.dirty = true;
    }

    /// Ctrl+B: the changed-files panel (needs a wide enough terminal).
    pub fn toggle_files(&mut self, term_width: u16) {
        if !self.show_files && term_width < FILES_PANEL_MIN_W {
            self.flash_level(Level::Warn, &format!("the files panel needs {FILES_PANEL_MIN_W} columns"));
            return;
        }
        self.show_files = !self.show_files;
        self.dirty = true;
    }

    // ------------------------------------------------------------ selection

    /// The transcript position under screen `(x, y)`; rows above or below the
    /// transcript map to the lines just outside the view.
    pub fn pos_at(&self, x: u16, y: u16) -> Pos {
        let a = self.transcript_area;
        let top = self.view_top();
        let line = if y < a.y {
            top.saturating_sub(1)
        } else if y >= a.y + a.height {
            top + a.height as usize
        } else {
            top + (y - a.y) as usize
        };
        let line = line.min(self.total_lines.saturating_sub(1));
        let col = x.saturating_sub(a.x) as usize;
        Pos { line, col }
    }

    /// The left button went down at `(x, y)`. A second and a third click in the same place
    /// select a word and a whole box or line.
    pub fn press_at(&mut self, x: u16, y: u16) {
        self.press = Some((x, y));
        if self.selection.as_ref().is_some_and(|s| !s.dragging) {
            self.selection = None;
            self.dirty = true;
        }
        if !crate::modal::contains(self.transcript_area, x, y) {
            self.tclick = None;
            return;
        }
        let pos = self.pos_at(x, y);
        let now = self.now();
        let n = match self.tclick {
            Some((t, p, n))
                if now.duration_since(t) < Duration::from_millis(400)
                    && p.line == pos.line
                    && p.col.abs_diff(pos.col) <= 1 =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.tclick = Some((now, pos, n));
        if n >= 2 {
            self.multi_select(pos, n);
        }
    }

    /// The box that holds absolute line `line`, if it sits in one.
    pub fn region_at(&mut self, line: usize) -> Option<RegionRef> {
        let i = self.block_at_line(line)?;
        let w = self.last_width;
        self.lines(i, w);
        let meta = &self.cache[i].as_ref()?.3;
        let idx = meta.region_of(line - self.offsets[i])?;
        let r = &meta.regions[idx];
        Some(RegionRef {
            block: i,
            idx,
            start: r.start as usize,
            end: r.end as usize,
            deco: r.deco as usize,
            kind: r.kind,
        })
    }

    /// The first and last absolute lines of a box (read again from the layout, which may have
    /// moved it since the press).
    pub(super) fn region_bounds(&self, r: &RegionRef) -> (usize, usize) {
        let off = self.offsets.get(r.block).copied().unwrap_or(0);
        let (s, e) = match self.cache.get(r.block).and_then(|c| c.as_ref()).and_then(|c| c.3.regions.get(r.idx)) {
            Some(g) if g.kind == r.kind => (g.start as usize, g.end as usize),
            _ => (r.start, r.end),
        };
        (off + s, off + e)
    }

    /// `p` held inside the box `r`: past its ends it goes to the start or the end of it.
    pub(super) fn clamp_to(&self, r: &RegionRef, p: Pos) -> Pos {
        let (first, last) = self.region_bounds(r);
        if p.line < first {
            Pos { line: first, col: r.deco }
        } else if p.line > last {
            Pos { line: last, col: usize::MAX - 1 }
        } else {
            Pos { line: p.line, col: p.col.max(r.deco) }
        }
    }

    /// What the layout knows of absolute line `line`, for a clean copy.
    pub(super) fn line_info(&mut self, line: usize) -> LineInfo {
        let Some(i) = self.block_at_line(line) else { return LineInfo::default() };
        let w = self.last_width;
        self.lines(i, w);
        let j = line - self.offsets[i];
        let Some((_, _, Some(lines), meta)) = self.cache[i].as_ref() else { return LineInfo::default() };
        let Some(l) = lines.get(j) else { return LineInfo::default() };
        let mut info = LineInfo {
            text: crate::render::markdown::text_of(l),
            cont: meta.cont_of(j),
            margin: meta.margin as usize,
            ..Default::default()
        };
        if let Some(idx) = meta.region_of(j) {
            let r = &meta.regions[idx];
            info.region = Some((i, idx));
            info.deco = r.deco as usize;
            info.skip = r.skip.contains(&(j as u32));
            info.whole = r.kind == crate::regions::Kind::Table;
            info.src = r.src.as_ref().and_then(|s| s.get(j - r.start as usize).cloned());
        }
        info
    }

    /// The text the selection copies.
    pub fn selection_text(&mut self) -> Option<String> {
        let sel = self.selection.clone()?;
        let (s, e) = sel.range();
        let infos: Vec<LineInfo> = (s.line..=e.line).map(|l| self.line_info(l)).collect();
        Some(sel.extract_clean(&infos))
    }

    /// A double click (a word) or a triple click (the box, or the logical line) on `pos`.
    /// Rows that are neither prose nor in a box (heads, summaries) are left to the click.
    pub(super) fn multi_select(&mut self, pos: Pos, n: u8) {
        let free = self.press_free;
        let region = if free { None } else { self.region_at(pos.line) };
        let Some(block) = self.block_at_line(pos.line) else { return };
        let prose = matches!(self.blocks[block], Block::User { .. } | Block::Assistant { .. } | Block::Notice { .. });
        if region.is_none() && !prose {
            return;
        }
        let info = self.line_info(pos.line);
        let mut sel = Selection::new(pos);
        sel.free = free;
        sel.fixed = true;
        if n == 2 {
            let lo = match (&region, info.cont) {
                (Some(r), _) => r.deco,
                (None, Some(c)) => c.deco as usize,
                (None, None) => {
                    crate::select::width_of(&info.text)
                        - crate::select::width_of(&crate::select::strip_rail(&info.text))
                }
            };
            let Some((a, b)) = crate::select::word_bounds(&info.text, pos.col, lo) else { return };
            sel.anchor = Pos { line: pos.line, col: a };
            sel.head = Pos { line: pos.line, col: b - 1 };
        } else if let Some(r) = region {
            let (first, last) = self.region_bounds(&r);
            sel.anchor = Pos { line: first, col: r.deco };
            sel.head = Pos { line: last, col: usize::MAX - 1 };
        } else {
            let (mut a, mut b) = (pos.line, pos.line);
            while a > 0 && self.line_info(a).cont.is_some() {
                a -= 1;
            }
            while b + 1 < self.total_lines && self.line_info(b + 1).cont.is_some() {
                b += 1;
            }
            sel.anchor = Pos { line: a, col: 0 };
            sel.head = Pos { line: b, col: usize::MAX - 1 };
        }
        if let (Some(r), false) = (region, free) {
            sel.deco = r.deco;
            sel.region = Some(r);
        }
        self.selection = Some(sel);
        self.dirty = true;
    }

    /// The pointer moved with the button down.
    pub fn drag_to(&mut self, x: u16, y: u16) {
        let Some((px, py)) = self.press else { return };
        if self.selection.as_ref().is_some_and(|s| s.fixed) {
            // a quick click then a drag: once the pointer leaves the cell, the drag is a plain one
            if (x, y) == (px, py) {
                return;
            }
            self.selection = None;
        }
        if self.selection.is_none() {
            if !crate::modal::contains(self.transcript_area, px, py) {
                return;
            }
            let anchor = self.pos_at(px, py);
            let mut sel = Selection::new(anchor);
            if self.press_free {
                sel.free = true;
            } else if let Some(r) = self.region_at(anchor.line) {
                sel.anchor = self.clamp_to(&r, anchor);
                sel.head = sel.anchor;
                sel.deco = r.deco;
                sel.region = Some(r);
            }
            self.selection = Some(sel);
        }
        let a = self.transcript_area;
        if y < a.y {
            self.scroll_by(-1);
        } else if y >= a.y + a.height {
            self.scroll_by(1);
        }
        self.drag_pos = Some((x, y));
        let mut head = self.pos_at(x, y);
        if let Some(r) = self.selection.as_ref().and_then(|s| s.region) {
            head = self.clamp_to(&r, head);
        }
        if let Some(s) = &mut self.selection {
            s.head = head;
            s.dragging = true;
        }
        self.dirty = true;
    }

    /// After a wheel scroll while dragging, the head follows the pointer.
    pub fn extend_after_scroll(&mut self) {
        if let (Some((x, y)), true) = (self.drag_pos, self.selection.as_ref().is_some_and(|s| s.dragging && !s.fixed)) {
            let mut head = self.pos_at(x, y);
            if let Some(r) = self.selection.as_ref().and_then(|s| s.region) {
                head = self.clamp_to(&r, head);
            }
            if let Some(s) = &mut self.selection {
                s.head = head;
            }
        }
    }

    /// A click on line `j` of block `i`: the first click selects it; a click on the head
    /// (or the `▸ more lines` under a folded one) of the selected block folds or opens it.
    pub fn click_block(&mut self, i: usize, j: usize) {
        let n = self.lines(i, self.last_width).len();
        let on_head = j == 0 || (self.is_folded(i) && j + 1 == n);
        if self.selected == Some(i) && on_head {
            self.toggle_block(i);
        } else {
            self.selected = Some(i);
        }
        self.dirty = true;
    }

    /// The button came up: a drag copies its selection, a click selects or toggles a block.
    pub fn release_at(&mut self, x: u16, y: u16) {
        let press = self.press.take();
        self.drag_pos = None;
        let dragged = self.selection.as_ref().is_some_and(|s| s.dragging && !s.is_empty());
        if let Some(s) = &mut self.selection {
            s.dragging = false;
        }
        if dragged {
            self.copy_selection();
            return;
        }
        self.selection = None;
        let Some((px, py)) = press else { return };
        let area = self.transcript_area;
        if crate::modal::contains(area, px, py) && crate::modal::contains(area, x, y) {
            let line = self.view_top() + (py - area.y) as usize;
            if let Some(i) = self.block_at_line(line) {
                self.click_block(i, line - self.offsets[i]);
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ pointer

    /// Notes a palette or a dialog that came up since the last frame: it pops in.
    pub fn note_popups(&mut self) {
        let now = self.now();
        let (p, m) = (self.palette.is_some(), self.modal.is_some());
        // a dialog that just came up starts from its own size, not the last one's
        if p && !self.palette_was {
            self.dialog_size.palette.forget();
        }
        if m && !self.modal_was {
            self.dialog_size.modal.forget();
        }
        if self.opts.animations {
            if p && !self.palette_was {
                self.palette_at = Some(now);
            }
            if m && !self.modal_was {
                self.modal_at = Some(now);
            }
            if (p && !self.palette_was) || (m && !self.modal_was) {
                self.moving = true;
            }
        }
        if self.opts.animations && self.fx.ghost.is_some() {
            if (self.palette_was && !p) || (self.modal_was && !m) {
                self.fx.ghost_at = Some(now);
                // the leaving is drawn frame by frame: the loop must not rest on its first frame
                self.moving = true;
            } else if p || m {
                self.fx.ghost_at = None;
            }
        }
        if !p {
            self.fx.pal_sel.clear();
            self.dialog_size.palette.forget();
        }
        if !m {
            self.dialog_size.modal.forget();
        }
        if !m {
            self.fx.pick_sel.clear();
        }
        if !m {
            self.modal_bar = None;
            if self.bar_drag.is_some_and(|d| d.target == scrollbar::Target::Modal) {
                self.bar_drag = None;
            }
        }
        (self.palette_was, self.modal_was) = (p, m);
    }

    /// 0..1: how far the dialog on top has popped open.
    pub fn modal_pop(&self) -> f64 {
        anim::ease_out(anim::progress(self.modal_at, self.now(), anim::POP_MS))
    }

    /// 0..1: how far the palette has risen.
    pub fn palette_pop(&self) -> f64 {
        anim::ease_out(anim::progress(self.palette_at, self.now(), anim::POP_MS))
    }

    /// The first transcript line in sight: where the glide is now, else `scroll`.
    pub fn view_top(&self) -> usize {
        let now = self.now();
        let v = if self.scroll_fx.is_moving(now) {
            self.scroll_fx.value_at(now).round().max(0.0) as usize
        } else {
            self.scroll
        };
        v.min(self.max_scroll(self.transcript_area.height as usize))
    }

    /// One frame of the glide towards `scroll` (the draw calls it once the scroll is
    /// settled): the first line to show. A jump while following the end that is
    /// longer than the view (a session loaded, a resize) lands at once, and so does
    /// everything with the animations off.
    pub fn glide(&mut self, height: usize) -> usize {
        let now = self.now();
        let target = self.scroll as f64;
        let far = (self.scroll_fx.value_at(now) - target).abs() > height as f64;
        let wished = std::mem::take(&mut self.wished);
        if !self.opts.animations || (far && !wished) {
            self.scroll_fx.snap(target);
        } else {
            self.scroll_fx.retarget(target, now);
        }
        let unseen = self.unseen();
        if unseen > self.unseen_before && self.opts.animations {
            self.unseen_at = Some(now);
        }
        self.unseen_before = unseen;
        (self.scroll_fx.value_at(now).round().max(0.0) as usize).min(self.max_scroll(height))
    }

    /// Puts transcript line `off` at the top: gliding there, or at once (a dragged thumb
    /// follows the pointer one to one).
    pub fn scroll_to_line(&mut self, off: usize, glide: bool) {
        let max = self.max_scroll(self.transcript_area.height as usize);
        let off = off.min(max);
        self.scroll = off;
        self.follow = off >= max;
        if self.follow {
            self.seen_lines = self.total_lines;
        }
        if glide {
            self.wished = true;
        } else {
            self.scroll_fx.snap(off as f64);
        }
        self.dirty = true;
    }

    /// The bar a press or a drag goes to: the dialog's while one is open.
    pub(super) fn live_bar(&self, target: Option<scrollbar::Target>) -> Option<Bar> {
        let bar = match target {
            Some(scrollbar::Target::Modal) => self.modal_bar,
            Some(scrollbar::Target::Transcript) => self.transcript_bar,
            None if self.modal.is_some() => self.modal_bar,
            None => self.transcript_bar,
        };
        bar.filter(|b| b.scrollable())
    }

    /// The left button went down at `(x, y)`: on a scrollbar, its thumb is held (a
    /// press on the track jumps there first). True when a bar took the press.
    pub fn bar_press(&mut self, x: u16, y: u16) -> bool {
        let Some(bar) = self.live_bar(None) else { return false };
        let Some((off, grab)) = bar.press(x, y) else { return false };
        let on_thumb = matches!(bar.hit(x, y), Some(Hit::Thumb { .. }));
        self.bar_drag = Some(scrollbar::Drag { target: bar.target, grab });
        self.press = None;
        if bar.target == scrollbar::Target::Transcript {
            self.selection = None;
        }
        self.set_bar_offset(bar, off, !on_thumb);
        self.dirty = true;
        true
    }

    /// The pointer moved with a thumb held: the scroll follows it. True when a thumb is held.
    pub fn bar_drag_to(&mut self, y: u16) -> bool {
        let Some(d) = self.bar_drag else { return false };
        if let Some(bar) = self.live_bar(Some(d.target)) {
            let off = bar.drag(y, d.grab);
            self.set_bar_offset(bar, off, false);
        }
        true
    }

    /// The button came up: a held thumb is let go. True when one was held.
    pub fn bar_release(&mut self) -> bool {
        if self.bar_drag.take().is_some() {
            self.dirty = true;
            return true;
        }
        false
    }

    pub(super) fn set_bar_offset(&mut self, bar: Bar, off: usize, glide: bool) {
        match bar.target {
            scrollbar::Target::Transcript => self.scroll_to_line(off, glide),
            scrollbar::Target::Modal => {
                let first = self.modal_list_first;
                if let Some(m) = &mut self.modal {
                    m.set_scroll(off, bar.viewport, first);
                }
                self.sync_theme_preview();
                self.dirty = true;
            }
        }
    }

    /// Whether the scrollbar of `target` is lit: under the pointer or held.
    pub fn bar_hot(&self, target: scrollbar::Target) -> bool {
        self.hover == Hover::Bar(target) || self.bar_drag.is_some_and(|d| d.target == target)
    }

    /// What is under `(x, y)`, topmost first.
    pub fn hover_at(&mut self, x: u16, y: u16) -> Hover {
        use crate::modal::contains;
        if self.modal.is_some() {
            if self.modal_bar.is_some_and(|b| b.scrollable() && b.contains(x, y)) {
                return Hover::Bar(scrollbar::Target::Modal);
            }
            if let Some((_, i)) = self.picker_rows.iter().find(|(r, _)| contains(*r, x, y)) {
                return Hover::Picker(*i);
            }
            return Hover::Nothing;
        }
        if self.palette.is_some() {
            if let Some((_, i)) = self.palette_rows.iter().find(|(r, _)| contains(*r, x, y)) {
                return Hover::Palette(*i);
            }
        }
        if self.transcript_bar.is_some_and(|b| b.scrollable() && b.contains(x, y)) {
            return Hover::Bar(scrollbar::Target::Transcript);
        }
        if self.unseen() > 0 && contains(self.pill, x, y) {
            return Hover::Pill;
        }
        if let Some((_, h)) = self.status_hits.iter().find(|(r, _)| contains(*r, x, y)) {
            return Hover::Status(*h);
        }
        if let Some((_, h)) = self.hint_hits.iter().find(|(r, _)| contains(*r, x, y)) {
            return Hover::Hint(*h);
        }
        let a = self.transcript_area;
        if contains(a, x, y) {
            let line = self.view_top() + (y - a.y) as usize;
            if let Some(i) = self.block_at_line(line) {
                if line == self.offsets[i] && self.foldable(i) {
                    return Hover::Head(i);
                }
            }
        }
        Hover::Nothing
    }

    /// The pointer moved to `(x, y)` with no button down: what it is over lights up.
    pub fn pointer_moved(&mut self, x: u16, y: u16) {
        let h = self.hover_at(x, y);
        if h != self.hover {
            self.hover = h;
            self.dirty = true;
        }
    }

    /// The status bar's piece at column `x`, when it answers a click.
    pub fn status_hit(&self, x: u16) -> Option<StatusHit> {
        let y = self.status_row;
        self.status_hits.iter().find(|(r, _)| crate::modal::contains(*r, x, y)).map(|(_, h)| *h)
    }

    /// The plain text of absolute line `line` of the laid-out transcript.
    pub fn line_text(&mut self, line: usize) -> Option<String> {
        let i = self.block_at_line(line)?;
        let off = self.offsets[i];
        let width = self.last_width;
        self.lines(i, width).get(line - off).map(crate::render::markdown::text_of)
    }

    pub fn copy_selection(&mut self) {
        let Some(sel) = self.selection.clone() else { return };
        let text = self.selection_text().unwrap_or_default();
        let n = sel.lines();
        match crate::clipboard::copy(&text) {
            Ok(how) => self.flash_toast(&format!("copied {n} line{} ({how})", if n == 1 { "" } else { "s" })),
            Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
        }
    }

    /// A press in the editor's text: the cursor goes there and a drag selects.
    pub fn editor_press(&mut self, x: u16, y: u16) {
        let a = self.editor_area;
        let pos = crate::input::cell_at(
            &self.input,
            a.width as usize,
            self.editor_top + (y - a.y) as usize,
            (x - a.x) as usize,
        );
        let now = self.now();
        let n = match self.editor_click {
            Some((t, p, n))
                if now.duration_since(t) < Duration::from_millis(400) && p.0 == pos.0 && p.1.abs_diff(pos.1) <= 1 =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.editor_click = Some((now, pos, n));
        match n {
            2 => self.input.select_word(pos),
            3 => self.input.select_line(pos.0),
            _ => self.input.press_at(pos),
        }
        self.editor_drag = true;
        self.dirty = true;
    }

    pub fn editor_drag_to(&mut self, x: u16, y: u16) {
        let a = self.editor_area;
        let (x, y) = (x.clamp(a.x, a.x + a.width.saturating_sub(1)), y.clamp(a.y, a.y + a.height.saturating_sub(1)));
        let pos = crate::input::cell_at(
            &self.input,
            a.width as usize,
            self.editor_top + (y - a.y) as usize,
            (x - a.x) as usize,
        );
        self.input.drag_to(pos);
        self.dirty = true;
    }

    /// The button comes up after a drag in the editor: what it selected is copied.
    pub fn editor_release(&mut self) {
        self.editor_drag = false;
        if let Some(text) = self.input.release_selection() {
            let n = text.lines().count().max(1);
            match crate::clipboard::copy(&text) {
                Ok(how) => self.flash_toast(&format!("copied {n} line{} ({how})", if n == 1 { "" } else { "s" })),
                Err(e) => self.notice(Level::Warn, format!("copy failed: {e}")),
            }
        }
        self.dirty = true;
    }
}
