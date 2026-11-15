//! The summary request: asking the model, and replacing the older history with its answer.

use super::*;
use crate::client::{ModelClient, Target};
use futures::StreamExt;
use nexo_core::message::{Message, Role};
use nexo_core::provider::ModelRequest;
use nexo_core::stream::{Collector, FinishReason, StreamEvent, Usage};
use std::sync::Arc;

pub struct Summariser {
    pub client: Arc<dyn ModelClient>,
    pub target: Target,
    pub retries: u32,
    pub max_tokens: u32,
    /// The model's context window in tokens, when known: the summary request and what the summary
    /// keeps word for word are sized to it.
    pub window: Option<u32>,
}

/// What a compaction did: the model's summary and what its request spent.
#[derive(Debug, Clone, Default)]
pub struct Compaction {
    pub summary: String,
    pub usage: Usage,
    pub model: String,
}

impl Summariser {
    /// One model call: the summary of `msgs`, and what the call spent.
    pub async fn summarise(&self, msgs: &[Message]) -> anyhow::Result<Compaction> {
        let (_, cfg, model) = self.target.get();
        let reply = self.window.map_or(self.max_tokens, |w| self.max_tokens.min(w / 2));
        let transcript = transcript(msgs, transcript_chars(self.window, reply));
        let ask = format!(
            "Transcript of the session so far:\n\n{transcript}\n\nWrite the summary. Your reply may be up to {reply} \
             tokens; a summary cut at that limit is thrown away."
        );
        let mut req = ModelRequest::new(model.clone(), SUMMARY_SYSTEM, vec![Message::user(ask)]);
        req.max_tokens = reply;
        req.cache = false;
        let mut stream = self.client.stream(&cfg, &req, self.retries).await?;
        let mut c = Collector::new();
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Error(e) = &ev {
                anyhow::bail!("summary failed: {e}");
            }
            c.push(ev);
        }
        if matches!(c.finish_reason(), FinishReason::Length | FinishReason::ContentFilter | FinishReason::Error) {
            anyhow::bail!("the summary came back cut short, so the history was kept as it was");
        }
        let text = c.message().text();
        if text.trim().is_empty() {
            anyhow::bail!("the summary came back empty");
        }
        Ok(Compaction { summary: text, usage: c.usage, model })
    }

    /// Tokens the kept messages of the person may take: a quarter of a known window at most.
    fn requests_tokens(&self) -> u64 {
        self.window.map_or(REQUESTS_TOKENS, |w| REQUESTS_TOKENS.min(u64::from(w) / 4))
    }
}

/// Replaces `history[..keep_from]` with its summary (see the module). Returns the summary and
/// what its request spent.
pub async fn compact(s: &Summariser, history: &mut Vec<Message>, keep_from: usize) -> anyhow::Result<Compaction> {
    let keep_from = keep_from.min(history.len());
    let done = s.summarise(&history[..keep_from]).await?;
    let mut k = kept(&history[..keep_from]);
    k.cap(s.requests_tokens());
    let parts = summary_parts(&k, &done.summary);
    let mut tail: Vec<Message> = history[keep_from..].to_vec();
    match tail.first_mut() {
        Some(m) if m.role == Role::User && !m.is_tool_results() => {
            m.content.splice(0..0, parts);
        }
        _ => tail.insert(0, Message::new(Role::User, parts)),
    }
    *history = tail;
    Ok(done)
}
