//! Explicit, local search of the displayed transcript, not a model request.
use super::*;

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub(super) struct ExploreState {
    sources: Arc<Vec<(usize, &'static str, String)>>,
    query: String,
    changed: Instant,
    generation: u64,
    submitted: bool,
    tx: mpsc::Sender<(u64, Vec<PickItem>)>,
    rx: mpsc::Receiver<(u64, Vec<PickItem>)>,
}

impl App {
    pub(super) fn explore(&mut self, query: Option<&str>) {
        // Bound work on the UI thread, and be explicit that this is a partial transcript.
        const MAX_BYTES: usize = 512 * 1024;
        const MAX_BLOCKS: usize = 2000;
        let mut sources = Vec::new();
        let mut bytes = 0usize;
        let mut partial = self.blocks.len() > MAX_BLOCKS;
        for (i, block) in self.blocks.iter().enumerate().rev().take(MAX_BLOCKS) {
            let (kind, text) = match block {
                Block::User { text, .. } => ("person", text.as_str()),
                Block::Assistant { text, .. } => ("assistant", text.as_str()),
                Block::Tool { output: Some(out), .. } if !out.is_error => ("tool output", out.content.as_str()),
                _ => continue,
            };
            if bytes.saturating_add(text.len()) > MAX_BYTES {
                partial = true;
                break;
            }
            bytes += text.len();
            sources.push((i, kind, text.to_owned()));
        }
        sources.reverse();
        let title = if partial {
            "Explorer · PREVIEW, not sent · partial transcript · shared keys, not proof"
        } else {
            "Explorer · PREVIEW, not sent · shared keys, not proof"
        };
        let mut picker = Picker::new(PickKind::Explore, title, vec![], None);
        picker.paste(query.unwrap_or_default());
        let (tx, rx) = mpsc::channel();
        self.explore_state = Some(ExploreState {
            sources: Arc::new(sources),
            query: picker.query.clone(),
            changed: Instant::now(),
            generation: 0,
            submitted: false,
            tx,
            rx,
        });
        self.modal = Some(Modal::Picker(picker));
    }

    pub(super) fn explore_tick(&mut self) {
        let Some(state) = &mut self.explore_state else { return };
        let Some(Modal::Picker(picker)) = &mut self.modal else {
            self.explore_state = None;
            return;
        };
        if picker.kind != PickKind::Explore {
            self.explore_state = None;
            return;
        }
        if picker.query != state.query {
            state.query.clone_from(&picker.query);
            state.generation = state.generation.wrapping_add(1);
            state.changed = Instant::now();
            state.submitted = false;
            picker.replace_items(vec![]);
            self.dirty = true;
        }
        while let Ok((generation, items)) = state.rx.try_recv() {
            if generation == state.generation {
                picker.replace_items(items);
                self.dirty = true;
            }
        }
        if !state.submitted && state.changed.elapsed() >= Duration::from_millis(150) {
            state.submitted = true;
            if state.query.trim().is_empty() {
                return;
            }
            let sources = Arc::clone(&state.sources);
            let query = state.query.clone();
            let generation = state.generation;
            let tx = state.tx.clone();
            std::thread::spawn(move || {
                let texts: Vec<_> = sources.iter().map(|s| s.2.as_str()).collect();
                let items = nexo_race::explore::search(&texts, &query)
                    .into_iter()
                    .map(|hit| {
                        let (i, kind, _) = &sources[hit.source];
                        let quote = crate::render::wrap::one_line(&hit.quote);
                        let lower = quote.to_lowercase();
                        let terms: Vec<_> =
                            query.split_whitespace().filter(|t| lower.contains(&t.to_lowercase())).collect();
                        let hint = if terms.is_empty() {
                            "Related by shared keys".into()
                        } else {
                            format!("Matched: {}", terms.join(", "))
                        };
                        PickItem { label: format!("#{i} {kind}:{} · {quote}", hit.line), value: i.to_string(), hint }
                    })
                    .collect();
                let _ = tx.send((generation, items));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::app;

    #[test]
    fn stale_generation_is_discarded() {
        let (mut app, _) = app();
        app.explore(Some("old"));
        let state = app.explore_state.as_mut().unwrap();
        state.tx.send((0, vec![PickItem { label: "stale".into(), value: "0".into(), hint: String::new() }])).unwrap();
        if let Some(Modal::Picker(p)) = &mut app.modal {
            p.query = "new".into();
        }
        app.explore_tick();
        assert!(matches!(&app.modal, Some(Modal::Picker(p)) if p.items.is_empty()));
    }
}
