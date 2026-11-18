//! Running one parsed action against the browser and shaping its answer.

use super::args::*;
use super::show::*;
use super::*;

impl BrowserTool {
    /// The current page, for the end of an answer: the snapshot, or why there is none.
    pub(super) async fn page(&self, b: &Browser) -> String {
        match b.snapshot().await {
            Ok(s) => format!("Page:\n{}", show_snapshot(&s)),
            Err(e) => format!("Page: snapshot failed: {e}"),
        }
    }

    pub(super) async fn act(&self, b: &Browser, action: Action<'_>, vision: bool, cwd: &Path) -> ToolOutput {
        match action {
            Action::Screenshot(full) => screenshot(b, full, vision).await,
            Action::Batch(steps) => self.batch(b, steps, cwd).await,
            action => match self.step(b, action, cwd).await {
                Ok(s) if s.page => ToolOutput::ok(format!("{}\n\n{}", s.text, self.page(b).await)),
                Ok(s) => ToolOutput::ok(s.text),
                // The page after a miss is what the model needs to pick a better target.
                Err(s) if s.page => ToolOutput::error(format!("browser: {}\n\n{}", s.text, self.page(b).await)),
                Err(s) => ToolOutput::error(s.text),
            },
        }
    }

    /// The steps of a `batch` in order, stopping at the first one that fails; a short log, then the page.
    pub(super) async fn batch(&self, b: &Browser, steps: Vec<(&str, Action<'_>)>, cwd: &Path) -> ToolOutput {
        let total = steps.len();
        let mut log = Vec::with_capacity(total + 1);
        for (i, (name, action)) in steps.into_iter().enumerate() {
            match self.step(b, action, cwd).await {
                Ok(s) => log.push(format!("{}. {name}: {}", i + 1, cap_text(&s.text, MAX_STEP_TEXT))),
                Err(s) => {
                    log.push(format!("{}. {name} FAILED: {}", i + 1, cap_text(&s.text, MAX_STEP_TEXT)));
                    log.push(format!("stopped at step {} of {total}; the rest did not run", i + 1));
                    return ToolOutput::error(format!("{}\n\n{}", log.join("\n"), self.page(b).await));
                }
            }
        }
        ToolOutput::ok(format!("{}\n\n{}", log.join("\n"), self.page(b).await))
    }

    /// One action. The text is what happened or what was read; `page` says whether the fresh page follows it.
    pub(super) async fn step(&self, b: &Browser, action: Action<'_>, cwd: &Path) -> Result<Step, Step> {
        let noted = gif::note(b, &action).await;
        let done = self.perform(b, action, cwd).await;
        // Where the browser sends no screencast frames, the recording still gets the page as this action left it.
        if noted {
            b.record_after_action().await;
        }
        done
    }

    async fn perform(&self, b: &Browser, action: Action<'_>, cwd: &Path) -> Result<Step, Step> {
        match action {
            Action::Gif(call) => gif::run(b, call, cwd).await,
            Action::CaptureStart => {
                b.capture_start();
                Ok(Step { text: capture::started(b.is_real()), page: false })
            }
            Action::CaptureStop => {
                let stats = b.capture_stop().await;
                Ok(Step { text: capture::stopped(&stats, b.is_real()), page: false })
            }
            Action::CaptureSave(path) => {
                let file = capture::target(cwd, path);
                let text = capture::write(&file, &b.export_har(), &b.capture_stats(), b.is_real())
                    .map_err(|e| Step { text: format!("browser capture_save: {e}"), page: false })?;
                Ok(Step { text, page: false })
            }
            Action::Navigate(url) => changed(b.navigate(url).await.map(|_| format!("opened {url}"))),
            Action::Snapshot => read("snapshot", b.snapshot().await.map(|s| show_snapshot(&s))),
            Action::ReadPage(a) => read("read_page", b.read_page(&a.options()).await.map(|t| show_tree(&t.text))),
            Action::ReadText(sel) => read("read_text", b.page_text(sel).await.map(|t| show_text(&t))),
            Action::Click(t) => changed(b.click(t).await.map(|_| format!("clicked {t}"))),
            Action::Fill(t, v) => changed(b.fill(t, v).await.map(|_| format!("filled {t}"))),
            Action::FillBatch(pairs) => {
                let report =
                    b.fill_batch(&pairs).await.map_err(|e| Step { text: format!("fill_batch: {e}"), page: false })?;
                let step = Step { text: fill_report(&report), page: true };
                if report.complete() {
                    Ok(step)
                } else {
                    Err(step)
                }
            }
            Action::Select(t, o) => changed(b.select(t, o).await.map(|_| format!("selected {o:?} in {t}"))),
            Action::SetCheckbox(t, on) => changed(
                b.set_checkbox(t, on).await.map(|_| format!("{} {t}", if on { "checked" } else { "unchecked" })),
            ),
            Action::Press(k) => changed(b.press(k).await.map(|_| format!("pressed {k}"))),
            Action::Hover(t) => changed(b.hover(t).await.map(|_| format!("hovered {t}"))),
            Action::Scroll(to) => changed(b.scroll(to).await.map(|_| format!("scrolled {to}"))),
            Action::Wait(sel, ms) => {
                let what = match sel {
                    Some(s) => Wait::Selector(s, ms.unwrap_or(WAIT_CAP_MS)),
                    None => Wait::Millis(ms.unwrap_or(0)),
                };
                changed(b.wait(what).await.map(|_| "wait over".to_string()))
            }
            Action::Back => changed(b.back().await.map(|_| "went back".to_string())),
            Action::Forward => changed(b.forward().await.map(|_| "went forward".to_string())),
            Action::Reload => changed(b.reload().await.map(|_| "reloaded".to_string())),
            Action::Eval(js) => read("eval", b.eval(js).await.map(|r| cap_text(&r, MAX_EVAL))),
            Action::Console(filter) => read("console", b.console(filter).await.map(|l| show_console(&l))),
            Action::Network(_, Some(n)) => read("network", b.network_detail(n).await.map(|d| show_request(&d))),
            Action::Network(filter, None) => read("network", b.network(filter).await.map(|l| show_network(&l))),
            Action::Find(query) => {
                read("find", b.find(query, MAX_FOUND).await.map(|(hits, n)| show_found(query, &hits, n)))
            }
            Action::Tabs => read("tabs", b.tabs().await.map(|t| show_tabs(&t))),
            Action::TabNew(url) => {
                let index = b.tab_new(url).await.map_err(|e| Step { text: e.to_string(), page: true })?;
                let tabs = b.tabs().await.map(|t| show_tabs(&t)).unwrap_or_default();
                Ok(Step { text: format!("opened {url} in tab {index}\n{tabs}"), page: true })
            }
            Action::TabSwitch(i) => changed(b.tab_switch(i).await.map(|_| format!("switched to tab {i}"))),
            Action::TabClose(i) => {
                b.tab_close(i).await.map_err(|e| Step { text: e.to_string(), page: true })?;
                let tabs = b.tabs().await.map(|t| show_tabs(&t)).unwrap_or_default();
                Ok(Step { text: format!("closed tab {i}\n{tabs}"), page: true })
            }
            Action::Upload(t, path) => {
                changed(b.upload(t, resolve(cwd, path)).await.map(|_| format!("uploaded {path} to {t}")))
            }
            Action::Resize(w, h) => changed(b.resize(w, h).await.map(|_| format!("resized to {w}x{h}"))),
            Action::ClickXy(x, y, m) => {
                changed(b.click_xy(x, y, m).await.map(|_| format!("clicked at {x},{y}{}", held(m))))
            }
            Action::DoubleClick(x, y, m) => {
                changed(b.double_click(x, y, m).await.map(|_| format!("double-clicked at {x},{y}{}", held(m))))
            }
            Action::RightClick(x, y, m) => {
                changed(b.right_click(x, y, m).await.map(|_| format!("right-clicked at {x},{y}{}", held(m))))
            }
            Action::Drag([x1, y1, x2, y2], m) => changed(b.drag((x1, y1), (x2, y2), m).await.map(|dropped| {
                let how = if dropped { ", dropped as a drag and drop" } else { "" };
                format!("dragged from {x1},{y1} to {x2},{y2}{}{how}", held(m))
            })),
            Action::HoverXy(x, y) => changed(b.hover_xy(x, y).await.map(|_| format!("moved the mouse to {x},{y}"))),
            Action::ScrollXy([x, y, dx, dy]) => {
                changed(b.scroll_xy(x, y, dx, dy).await.map(|_| format!("scrolled by {dx},{dy} at {x},{y}")))
            }
            Action::TypeText(t) => {
                changed(b.type_text(t).await.map(|_| format!("typed {} characters", t.chars().count())))
            }
            Action::Screenshot(_)
            | Action::Batch(_)
            | Action::Close
            | Action::Open(..)
            | Action::Browsers
            | Action::SelectBrowser(_)
            | Action::SwitchBrowser => Err(Step { text: "browser: this action cannot run here".into(), page: false }),
        }
    }
}

/// What one action answered: its text, and whether the fresh page follows it (every action that changes the page).
pub(super) struct Step {
    text: String,
    page: bool,
}

/// A page-changing action: its message, or why it failed (then the page shows what is there instead).
pub(super) fn changed(done: Result<String, nexo_browser::Error>) -> Result<Step, Step> {
    done.map(|text| Step { text, page: true }).map_err(|e| Step { text: e.to_string(), page: true })
}

/// A read-only action: its data alone, no page after it.
pub(super) fn read(name: &str, done: Result<String, nexo_browser::Error>) -> Result<Step, Step> {
    done.map(|text| Step { text, page: false }).map_err(|e| Step { text: format!("browser {name}: {e}"), page: false })
}

pub(super) async fn screenshot(b: &Browser, full: bool, vision: bool) -> ToolOutput {
    if !vision {
        return ToolOutput::error("browser screenshot: this model cannot view images; use snapshot or read_text");
    }
    match b.screenshot(full).await {
        Ok(png) if png.len() > MAX_SHOT => ToolOutput::error(format!(
            "browser screenshot: the picture is {} bytes, too large to show; use full=false or read_text",
            png.len()
        )),
        Ok(png) => {
            let data = base64::engine::general_purpose::STANDARD.encode(&png);
            ToolOutput::ok(format!(
                "screenshot of the {} ({} bytes)",
                if full { "full page" } else { "viewport" },
                png.len()
            ))
            .with_images(vec![ImageSource::Base64 { media_type: "image/png".into(), data }])
        }
        Err(e) => ToolOutput::error(format!("browser screenshot: {e}")),
    }
}
