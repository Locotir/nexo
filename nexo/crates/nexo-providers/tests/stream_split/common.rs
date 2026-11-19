use nexo_core::{Collector, StreamEvent};
use nexo_providers::anthropic::MessagesParser;
use nexo_providers::gemini::GeminiParser;
use nexo_providers::openai_chat::ChatParser;
use nexo_providers::openai_responses::ResponsesParser;
use nexo_providers::parse::Feed;
use nexo_providers::sse::{SseEvent, SseParser};

/// A small deterministic generator (xorshift64*), so every run sees the same cases.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// A number in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

pub const PROTOCOLS: [&str; 4] = ["anthropic", "openai-chat", "openai-responses", "gemini"];

pub fn fixture(protocol: &str) -> &'static str {
    match protocol {
        "anthropic" => include_str!("../fixtures/anthropic.sse"),
        "openai-chat" => include_str!("../fixtures/openai_chat.sse"),
        "openai-responses" => include_str!("../fixtures/openai_responses.sse"),
        "gemini" => include_str!("../fixtures/gemini.sse"),
        _ => panic!("unknown protocol {protocol}"),
    }
}

/// The events of `bytes` cut at `cuts` (sorted offsets), end of stream included.
pub fn sse_events(bytes: &[u8], cuts: &[usize]) -> Vec<SseEvent> {
    let mut p = SseParser::new();
    let mut out = Vec::new();
    let mut at = 0;
    for &c in cuts.iter().chain(std::iter::once(&bytes.len())) {
        out.extend(p.push(&bytes[at..c]));
        at = c;
    }
    out.extend(p.finish());
    out
}

/// The data of each event of a recorded stream, read in one chunk.
pub fn payloads(protocol: &str) -> Vec<String> {
    sse_events(fixture(protocol).as_bytes(), &[]).into_iter().map(|e| e.data).collect()
}

pub fn parser(protocol: &str) -> Box<dyn Feed> {
    match protocol {
        "anthropic" => Box::<MessagesParser>::default(),
        "openai-chat" => Box::<ChatParser>::default(),
        "openai-responses" => Box::<ResponsesParser>::default(),
        "gemini" => Box::<GeminiParser>::default(),
        _ => panic!("unknown protocol {protocol}"),
    }
}

/// What the parser makes of the payloads, then of the end of the stream.
pub fn drive(protocol: &str, data: &[String]) -> Vec<StreamEvent> {
    let mut p = parser(protocol);
    let mut out = Vec::new();
    for d in data {
        out.extend(p.feed(d));
    }
    out.extend(p.finish());
    out
}

pub fn collect(events: Vec<StreamEvent>) -> Collector {
    let mut c = Collector::new();
    for e in events {
        c.push(e);
    }
    c
}
