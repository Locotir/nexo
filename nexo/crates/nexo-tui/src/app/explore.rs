//! Explicit, local search of the displayed transcript, not a model request.
use super::*;

impl App {
    pub(super) fn explore(&mut self, query: Option<&str>) {
        let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
            self.notice(
                Level::Info,
                "/explore <phrase> · searches the displayed session; /sent shows the last prepared request",
            );
            return;
        };
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
            sources.push((i, kind, text));
        }
        sources.reverse();
        let texts: Vec<_> = sources.iter().map(|s| s.2).collect();
        let hits = nexo_curator::explore::search(&texts, query);
        let items = hits
            .into_iter()
            .map(|hit| {
                let (i, kind, _) = sources[hit.source];
                PickItem {
                    label: format!("#{i} {kind}:{} · {}", hit.line, crate::render::wrap::one_line(&hit.quote)),
                    value: i.to_string(),
                    hint: "Enter: source block".into(),
                }
            })
            .collect();
        let title = if partial {
            "Explorer · PREVIEW, not sent · partial transcript · shared keys, not proof"
        } else {
            "Explorer · PREVIEW, not sent · shared keys, not proof"
        };
        self.modal = Some(Modal::Picker(Picker::new(PickKind::Explore, title, items, None)));
    }
}
