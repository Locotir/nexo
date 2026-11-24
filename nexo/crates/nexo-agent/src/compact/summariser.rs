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
    /// The tokens a request of the session has for the conversation once compacted: the window less the fixed text
    /// (system prompt and tools) and the reply the session asks, when the window is known. The summary and what
    /// is kept word for word share it, so the request after the compaction fits.
    pub room: Option<u64>,
}

/// The least tokens a summary may be asked for, however small the room.
const LEAST_SUMMARY_TOKENS: u32 = 256;

/// What a compaction did: the model's summary and what its request spent.
#[derive(Debug, Clone, Default)]
pub struct Compaction {
    pub summary: String,
    pub usage: Usage,
    pub model: String,
}

/// Compaction cannot replace a prefix containing images with a text-only summary.
#[derive(Debug)]
pub struct ImagesInCompactionPrefix;

impl std::fmt::Display for ImagesInCompactionPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "history contains images that text-only compaction cannot safely preserve; history was kept as it was",
        )
    }
}

impl std::error::Error for ImagesInCompactionPrefix {}

/// A rejected response still spent these tokens. History replacement and billing are independent.
#[derive(Debug)]
pub struct CompactionFailure {
    pub usage: Usage,
    pub model: String,
    reason: String,
}

impl std::fmt::Display for CompactionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for CompactionFailure {}

fn rejected(reason: impl Into<String>, usage: Usage, model: &str) -> anyhow::Error {
    CompactionFailure { usage, model: model.to_owned(), reason: reason.into() }.into()
}

impl Summariser {
    /// One model call: the summary of `msgs`, and what the call spent.
    pub async fn summarise(&self, msgs: &[Message]) -> anyhow::Result<Compaction> {
        let (_, cfg, model) = self.target.get();
        let reply = self.window.map_or(self.max_tokens, |w| self.max_tokens.min(w / 2));
        // half of the room the compacted conversation has, so the kept messages and the newest ones fit with it
        let reply = match self.room {
            Some(room) => reply.min(u32::try_from(room / 2).unwrap_or(u32::MAX).max(LEAST_SUMMARY_TOKENS)),
            None => reply,
        };
        let mut req = summary_request(&model, msgs, self.window, reply)?;
        req.cache = false;
        // thinking would eat the reply cap and a cut summary is thrown away: the least the model takes
        req.reasoning = nexo_providers::probe::side_reasoning(&cfg, &model);
        let mut stream = self.client.stream(&cfg, &req, self.retries).await?;
        let mut c = Collector::new();
        while let Some(ev) = stream.next().await {
            if let StreamEvent::Error(e) = &ev {
                return Err(rejected(format!("summary failed: {e}"), c.usage, &model));
            }
            c.push(ev);
        }
        if matches!(c.finish_reason(), FinishReason::Length | FinishReason::ContentFilter | FinishReason::Error) {
            return Err(rejected(
                "the summary came back cut short, so the history was kept as it was",
                c.usage,
                &model,
            ));
        }
        let text = c.message().text();
        if text.trim().is_empty() {
            return Err(rejected("the summary came back empty", c.usage, &model));
        }
        // Subscription backends can reject a wire-level limit (Codex's omit_max_tokens).
        // A normal Stop is not proof that the reply obeyed our budget. Do not truncate it:
        // accepting a partial summary could lose the only record of unfinished work.
        if text.len() as u64 > u64::from(reply) * WINDOW_CHARS_PER_TOKEN || c.call.output > u64::from(reply) {
            return Err(rejected(
                "the summary exceeded its reply budget, so the history was kept as it was",
                c.usage,
                &model,
            ));
        }
        Ok(Compaction { summary: text, usage: c.usage, model })
    }

    /// Tokens the kept messages of the person may take: a quarter of a known window at most, and a quarter of
    /// the room the compacted conversation has.
    fn requests_tokens(&self) -> u64 {
        let by_window = self.window.map_or(REQUESTS_TOKENS, |w| REQUESTS_TOKENS.min(u64::from(w) / 4));
        self.room.map_or(by_window, |room| by_window.min(room / 4))
    }
}

/// Fit the complete summary request, not just its transcript. In particular, the fixed prompt,
/// instruction suffix and UTF-8 bytes must share the window with the exact reply cap. This is an
/// estimate (three bytes per token plus framing), not a provider tokenizer.
fn summary_request(model: &str, msgs: &[Message], window: Option<u32>, reply: u32) -> anyhow::Result<ModelRequest> {
    anyhow::ensure!(reply > 0, "summary not sent: no room for a reply");
    let mut chars = transcript_chars(window, reply);
    loop {
        anyhow::ensure!(chars > 0 || msgs.is_empty(), "summary not sent: no room for the transcript");
        let transcript = if chars == 0 { String::new() } else { transcript(msgs, chars) };
        let ask = format!(
            "Transcript of the session so far:\n\n{transcript}\n\nWrite the summary. Your reply may be up to {reply} \
             tokens; a summary cut at that limit is thrown away."
        );
        let mut req = ModelRequest::new(model, SUMMARY_SYSTEM, vec![Message::user(ask)]);
        req.max_tokens = reply;
        // Keep the cap even with an unknown window: modern Anthropic otherwise raises it
        // to 4096 (or 64000). Backends that omit wire limits are checked after the response.
        req.output_ceiling = Some(reply);
        if window.is_none_or(|w| summary_input_tokens(&req) + u64::from(reply) <= u64::from(w)) {
            return Ok(req);
        }
        anyhow::ensure!(chars > 0, "summary not sent: fixed instructions and reply exceed the context window");
        // Re-render from the originals to retain the task and newest work, including omission marks.
        // Those marks can exceed a tiny character budget; never silently summarise an empty view.
        chars /= 2;
    }
}

fn summary_input_tokens(req: &ModelRequest) -> u64 {
    let bytes = req.system.len() as u64 + req.messages.iter().map(|m| m.text().len() as u64).sum::<u64>();
    bytes.div_ceil(WINDOW_CHARS_PER_TOKEN) + 32
}

/// Replaces `history[..keep_from]` with its summary (see the module). Returns the summary and
/// what its request spent.
pub async fn compact(s: &Summariser, history: &mut Vec<Message>, keep_from: usize) -> anyhow::Result<Compaction> {
    let keep_from = keep_from.min(history.len());
    // A text-only summary cannot preserve the association between images and their original
    // messages (including tool results). Refuse before spending tokens or modifying history.
    fn has_image(p: &nexo_core::message::Part) -> bool {
        match p {
            nexo_core::message::Part::Image { .. } => true,
            nexo_core::message::Part::ToolResult { content, .. } => content.iter().any(has_image),
            _ => false,
        }
    }
    if history[..keep_from].iter().any(|m| m.content.iter().any(has_image)) {
        return Err(ImagesInCompactionPrefix.into());
    }
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
    // Validate the complete replacement, not just the kept requests: file paths, summary
    // framing and the untouched tail all share the conversation's room. Fail atomically rather
    // than silently dropping paths, tool results, or the current request to make it fit.
    if s.room.or(s.window.map(u64::from)).is_some_and(|room| nexo_core::message::estimate_tokens(&tail) > room) {
        return Err(rejected(
            "the compacted history exceeds its conversation budget, so the history was kept as it was",
            done.usage,
            &done.model,
        ));
    }
    *history = tail;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::config::ProviderConfig;
    use nexo_providers::anthropic::Anthropic;

    #[test]
    fn summary_caps_the_real_anthropic_body_even_without_a_known_window() {
        let cfg = ProviderConfig { protocol: "anthropic".into(), ..Default::default() };
        for window in [None, Some(2_048), Some(32_000)] {
            for reply in [256, 1_024] {
                let req = summary_request("claude-sonnet-5-5", &[Message::user("work")], window, reply).unwrap();
                let body = Anthropic::body(&cfg, &req);
                assert_eq!(req.output_ceiling, Some(reply));
                assert_eq!(body["max_tokens"], reply);
                if let Some(w) = window {
                    assert!(summary_input_tokens(&req) + u64::from(reply) <= u64::from(w));
                }
            }
        }
        // The default-size request used to trigger Anthropic's 64000-token floor.
        let reply = nexo_core::config::DEFAULT_MAX_TOKENS;
        let req = summary_request("claude-opus-5-5", &[], None, reply).unwrap();
        assert_eq!(Anthropic::body(&cfg, &req)["max_tokens"], reply);
    }

    #[test]
    fn summary_fits_fixed_instructions_and_multibyte_transcripts_before_sending() {
        let cfg = ProviderConfig { protocol: "anthropic".into(), ..Default::default() };
        for words in ["x", "漢", "🦀"] {
            let msgs = vec![Message::user(words.repeat(30_000)), Message::assistant("LATEST: tests pass")];
            for window in [512, 1_024, 4_096] {
                let reply = window / 2;
                let req = summary_request("claude-sonnet-5-5", &msgs, Some(window), reply).unwrap();
                let body = Anthropic::body(&cfg, &req);
                assert!(summary_input_tokens(&req) + body["max_tokens"].as_u64().unwrap() <= u64::from(window));
                assert!(req.messages[0].text().contains("LATEST: tests pass"));
            }
        }
    }

    struct Reply {
        text: String,
        output: u64,
    }

    #[async_trait::async_trait]
    impl ModelClient for Reply {
        async fn stream(
            &self,
            cfg: &ProviderConfig,
            req: &ModelRequest,
            _: u32,
        ) -> Result<nexo_core::provider::EventStream, nexo_core::error::ProviderError> {
            // Exercise the real subscription serializer: the server cannot accept our cap.
            let body = nexo_providers::openai_responses::OpenAiResponses::body(cfg, req);
            assert!(body.get("max_output_tokens").is_none());
            assert_eq!(req.output_ceiling, Some(req.max_tokens));
            Ok(Box::pin(futures::stream::iter(vec![
                StreamEvent::TextDelta(self.text.clone()),
                StreamEvent::Usage(Usage { output: self.output, ..Default::default() }),
                StreamEvent::Finish(FinishReason::Stop),
                StreamEvent::Done,
            ])))
        }
    }

    fn subscription_summary(text: String, output: u64, room: Option<u64>) -> Summariser {
        let mut cfg =
            ProviderConfig { protocol: "openai-responses".into(), login: Some("codex".into()), ..Default::default() };
        cfg.extra.insert("omit_max_tokens".into(), true.into());
        Summariser {
            client: Arc::new(Reply { text, output }),
            target: Target::new("codex", cfg, "gpt-5.4"),
            retries: 0,
            max_tokens: 256,
            window: Some(4_096),
            room,
        }
    }

    #[tokio::test]
    async fn subscription_summary_rejects_oversized_stop_without_losing_history() {
        // Reject by measured bytes (even with absent/under-reported usage), or vendor tokens.
        for (text, output) in [("x".repeat(769), 0), ("漢".repeat(257), 1), ("short".into(), 257)] {
            let s = subscription_summary(text, output, None);
            let mut history = vec![Message::user("original task"), Message::assistant("original work")];
            let before = serde_json::to_value(&history).unwrap();
            let err = compact(&s, &mut history, 2).await.unwrap_err();
            assert!(err.to_string().contains("reply budget"), "{err}");
            let failed = err.downcast_ref::<CompactionFailure>().unwrap();
            assert_eq!(failed.usage.output, output);
            assert_eq!(failed.model, "gpt-5.4");
            assert_eq!(serde_json::to_value(&history).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn oversized_file_index_or_tail_cannot_replace_history() {
        for files in [false, true] {
            let s = subscription_summary("valid summary".into(), 8, Some(512));
            let mut history = vec![Message::user("original task")];
            if files {
                let paths = (0..2_000).map(|i| format!("  src/unique_path_{i:04}.rs")).collect::<Vec<_>>().join("\n");
                history.push(Message::user(format!("{FILES}\nchanged:\n{paths}")));
            }
            let keep_from = history.len();
            history.push(Message::assistant(if files { "tail".into() } else { "tail".repeat(2_000) }));
            let before = serde_json::to_value(&history).unwrap();
            let err = compact(&s, &mut history, keep_from).await.unwrap_err();
            assert!(err.to_string().contains("conversation budget"), "{err}");
            assert_eq!(err.downcast_ref::<CompactionFailure>().unwrap().usage.output, 8);
            assert_eq!(serde_json::to_value(&history).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn complete_replacement_fits_and_preserves_both_tail_shapes() {
        for user_tail in [false, true] {
            let s = subscription_summary("valid summary".into(), 8, Some(512));
            let tail = if user_tail { Message::user("current request") } else { Message::assistant("latest work") };
            let mut history = vec![Message::user("original task"), tail.clone()];
            compact(&s, &mut history, 1).await.unwrap();
            assert!(nexo_core::message::estimate_tokens(&history) <= 512);
            assert!(history[0].text().contains("valid summary"));
            assert!(history[0].text().contains("original task"));
            if user_tail {
                assert!(history[0].text().ends_with(&tail.text()));
            } else {
                assert_eq!(serde_json::to_value(history.last().unwrap()).unwrap(), serde_json::to_value(tail).unwrap());
            }
        }
    }

    #[tokio::test]
    async fn compaction_refuses_images_without_changing_their_context() {
        use nexo_core::message::{ImageSource, Part};
        let image = Part::Image { source: ImageSource::Url { url: "https://example.invalid/task.png".into() } };
        let result = Part::ToolResult {
            call_id: "call".into(),
            name: Some("read".into()),
            content: vec![Part::text("screenshot"), image.clone()],
            is_error: false,
        };
        for message in [
            Message::new(Role::User, vec![Part::text("implement this"), image.clone()]),
            Message::new(Role::User, vec![result]),
            Message::new(Role::Assistant, vec![image.clone()]),
        ] {
            let original = vec![Message::user("first task"), message, Message::user("later task")];
            let mut history = original.clone();
            for _ in 0..2 {
                let err = compact(&subscription_summary("valid summary".into(), 8, Some(4_000)), &mut history, 2)
                    .await
                    .unwrap_err();
                assert!(err.downcast_ref::<ImagesInCompactionPrefix>().is_some(), "{err}");
                assert!(err.to_string().contains("images"), "{err}");
                assert_eq!(history, original);
            }
        }
        // Images in the untouched tail do not block text-only compaction.
        let tail = Message::new(Role::User, vec![Part::text("later task"), image]);
        let mut history = vec![Message::user("first task"), tail.clone()];
        compact(&subscription_summary("valid summary".into(), 8, Some(4_000)), &mut history, 1).await.unwrap();
        assert!(history[0].content.ends_with(&tail.content));
    }

    #[test]
    fn summary_rejects_impossible_windows_and_zero_replies() {
        assert!(summary_request("claude-sonnet-5-5", &[], Some(128), 64).is_err());
        assert!(summary_request("claude-sonnet-5-5", &[], Some(1_024), 0).is_err());
        assert!(summary_request("claude-sonnet-5-5", &[], None, 0).is_err());
    }
}
