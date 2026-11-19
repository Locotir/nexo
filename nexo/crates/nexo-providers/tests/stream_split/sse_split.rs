//! Property: the events of an SSE stream do not depend on where the network cut the bytes.

use bytes::Bytes;
use futures::StreamExt;
use nexo_providers::sse::{SseEvent, SseParser, SseStream};

use crate::common::{fixture, sse_events, Rng, PROTOCOLS};

/// Sorted distinct random cut offsets inside `0..len`.
fn random_cuts(rng: &mut Rng, len: usize, count: usize) -> Vec<usize> {
    if len < 2 {
        return Vec::new();
    }
    let mut cuts: Vec<usize> = (0..count).map(|_| 1 + rng.below(len - 1)).collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// Checks `bytes` whole against every single cut, every byte on its own, and `random` seeded multi-cut layouts.
fn assert_split_invariant(name: &str, bytes: &[u8], every_cut: bool, random: usize, seed: u64) -> Vec<SseEvent> {
    let whole = sse_events(bytes, &[]);
    if every_cut {
        for c in 1..bytes.len() {
            assert_eq!(sse_events(bytes, &[c]), whole, "{name}: one cut at byte {c} of {}", bytes.len());
        }
    }
    let each: Vec<usize> = (1..bytes.len()).collect();
    assert_eq!(sse_events(bytes, &each), whole, "{name}: one byte per chunk");
    let mut rng = Rng::new(seed);
    for round in 0..random {
        let n = 1 + rng.below(12);
        let cuts = random_cuts(&mut rng, bytes.len(), n);
        assert_eq!(sse_events(bytes, &cuts), whole, "{name}: seed {seed} round {round} cuts {cuts:?}");
    }
    whole
}

fn with_line_ends(text: &str, end: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', end)
}

#[test]
fn recorded_streams_give_the_same_events_cut_at_every_position_with_each_line_end() {
    for (i, protocol) in PROTOCOLS.iter().enumerate() {
        let base = fixture(protocol);
        let reference = sse_events(base.as_bytes(), &[]);
        let datas = base.lines().filter(|l| l.starts_with("data:")).count();
        assert!(datas >= 3, "{protocol}: the fixture has events");
        assert_eq!(reference.len(), datas, "{protocol}: every data line is one event");
        for (j, end) in ["\n", "\r\n", "\r"].iter().enumerate() {
            let text = with_line_ends(base, end);
            let name = format!("{protocol} with line end {j}");
            let events = assert_split_invariant(&name, text.as_bytes(), true, 40, 100 + (i * 3 + j) as u64);
            assert_eq!(events, reference, "{name}: the line end changes nothing");
        }
    }
}

#[test]
fn recorded_streams_with_multibyte_text_survive_a_cut_inside_a_character() {
    for protocol in PROTOCOLS {
        let text = fixture(protocol).replace("file", "fichier \u{e9}\u{65e5}\u{672c}\u{1f600}");
        assert!(text.len() > fixture(protocol).len());
        assert_split_invariant(protocol, text.as_bytes(), true, 20, 7);
        let events = sse_events(text.as_bytes(), &[]);
        assert!(events.iter().any(|e| e.data.contains('\u{1f600}')), "{protocol}: the text is in the events");
        assert!(
            events.iter().all(|e| !e.data.contains('\u{fffd}')),
            "{protocol}: no cut makes a replacement character"
        );
    }
}

/// One block of a generated stream.
#[derive(Debug, Default, Clone)]
struct Block {
    comment: Option<String>,
    event: Option<String>,
    id: Option<String>,
    retry: bool,
    unknown: bool,
    /// `None`: a bare `data` line without a colon.
    data: Vec<Option<String>>,
}

const WORDS: [&str; 12] = [
    "a",
    "hello",
    "\u{e9}",
    "\u{65e5}\u{672c}\u{8a9e}",
    "\u{1f600}",
    " lead",
    "x:y",
    "{\"k\":1}",
    "",
    "tail ",
    "[DONE]",
    "d\u{300}",
];

fn text(rng: &mut Rng) -> String {
    (0..1 + rng.below(4)).map(|_| *rng.pick(&WORDS)).collect()
}

fn block(rng: &mut Rng) -> Block {
    let mut b = Block::default();
    if rng.chance(15) {
        b.comment = Some(text(rng));
    }
    if rng.chance(40) {
        b.event = Some(format!("ev{}", rng.below(5)));
    }
    if rng.chance(20) {
        b.id = Some(format!("{}", rng.below(1000)));
    }
    b.retry = rng.chance(10);
    b.unknown = rng.chance(10);
    // one block in six has no data at all: an empty or keep-alive event
    let lines = if rng.chance(17) { 0 } else { 1 + rng.below(3) };
    for _ in 0..lines {
        b.data.push(if rng.chance(8) { None } else { Some(text(rng)) });
    }
    b
}

/// The line terminator for a style: 0 `\n`, 1 `\r\n`, 2 `\r`, 3 chosen per line.
fn terminator(style: usize, rng: &mut Rng, prev_cr: bool, empty: bool) -> &'static str {
    let t = match style {
        0 => "\n",
        1 => "\r\n",
        2 => "\r",
        _ => *rng.pick(&["\n", "\r\n", "\r"]),
    };
    // an empty line after a lone `\r` must not start with `\n`: `\r\n` would be one terminator
    if prev_cr && empty && t == "\n" {
        return "\r";
    }
    t
}

/// The stream and the events it must give; `tail` leaves the last block unterminated.
fn render(rng: &mut Rng, blocks: &[Block], style: usize, tail: usize) -> (Vec<u8>, Vec<SseEvent>) {
    let mut lines: Vec<String> = Vec::new();
    let mut want = Vec::new();
    let mut last_id: Option<String> = None;
    for b in blocks {
        if let Some(c) = &b.comment {
            lines.push(format!(": {c}"));
        }
        if let Some(e) = &b.event {
            lines.push(format!("event: {e}"));
        }
        if let Some(i) = &b.id {
            lines.push(format!("id: {i}"));
            last_id = Some(i.clone());
        }
        if b.retry {
            lines.push("retry: 1500".into());
        }
        if b.unknown {
            lines.push("x-note: ignored".into());
        }
        for d in &b.data {
            lines.push(match d {
                Some(d) => format!("data: {d}"),
                None => "data".into(),
            });
        }
        if !b.data.is_empty() {
            let data: Vec<&str> = b.data.iter().map(|d| d.as_deref().unwrap_or("")).collect();
            want.push(SseEvent { event: b.event.clone(), data: data.join("\n"), id: last_id.clone() });
        }
        lines.push(String::new());
    }
    // tail 0: everything terminated; 1: drop the last blank line; 2: also the last line's terminator;
    // 3: the last line ends with a lone `\r` and nothing after it
    let mut out = String::new();
    let mut prev_cr = false;
    let n = lines.len();
    for (k, line) in lines.iter().enumerate() {
        let last = k + 1 == n;
        if last && tail >= 1 {
            break; // the blank line that closes the final block
        }
        let second_last = k + 2 == n;
        out.push_str(line);
        if second_last && tail == 2 {
            break;
        }
        let t = if second_last && tail == 3 { "\r" } else { terminator(style, rng, prev_cr, line.is_empty()) };
        prev_cr = t == "\r";
        out.push_str(t);
    }
    (out.into_bytes(), want)
}

fn generated(seed: u64) -> (String, Vec<u8>, Vec<SseEvent>) {
    let mut rng = Rng::new(seed);
    let n = 1 + rng.below(6);
    let blocks: Vec<Block> = (0..n).map(|_| block(&mut rng)).collect();
    let style = rng.below(4);
    let tail = if rng.chance(60) { 0 } else { rng.below(4) };
    let (bytes, want) = render(&mut rng, &blocks, style, tail);
    (format!("seed {seed} style {style} tail {tail}"), bytes, want)
}

#[test]
fn generated_streams_give_the_same_events_at_every_cut_and_the_events_that_were_written() {
    let mut exhaustive = 0;
    for seed in 1..=300u64 {
        let (name, bytes, want) = generated(seed);
        let small = bytes.len() <= 400 && exhaustive < 80;
        if small {
            exhaustive += 1;
        }
        let got = assert_split_invariant(&name, &bytes, small, 12, seed);
        assert_eq!(got, want, "{name}: the events written are the events read\n{:?}", String::from_utf8_lossy(&bytes));
    }
}

/// Regression: a stream that ends on a lone `\r` (no `\n` after it) has that `\r` as its last line
/// terminator, not as part of the line.
#[test]
fn regression_a_final_lone_cr_terminates_the_last_line_instead_of_joining_its_data() {
    for bytes in [&b"data: a\r"[..], b"data: a\r\r", b"event: e\rdata: a\r"] {
        let events = sse_events(bytes, &[]);
        assert_eq!(events.len(), 1, "{:?}", String::from_utf8_lossy(bytes));
        assert_eq!(events[0].data, "a", "{:?}", String::from_utf8_lossy(bytes));
        let each: Vec<usize> = (1..bytes.len()).collect();
        assert_eq!(sse_events(bytes, &each), events);
    }
}

async fn through_the_stream(bytes: &[u8], cuts: &[usize]) -> Vec<Result<SseEvent, String>> {
    let mut chunks = Vec::new();
    let mut at = 0;
    for &c in cuts.iter().chain(std::iter::once(&bytes.len())) {
        chunks.push(Ok(Bytes::copy_from_slice(&bytes[at..c])));
        at = c;
    }
    SseStream::new(futures::stream::iter(chunks)).collect().await
}

#[tokio::test]
async fn the_byte_stream_layer_gives_the_events_of_the_parser_at_seeded_cuts_inside_characters() {
    let mut inputs: Vec<(String, Vec<u8>)> =
        PROTOCOLS.iter().map(|p| (p.to_string(), fixture(p).as_bytes().to_vec())).collect();
    inputs.push((
        "multibyte".into(),
        "data: \u{65e5}\u{672c}\u{1f600}\r\n\r\ndata: \u{e9}\r\rdata: z".as_bytes().to_vec(),
    ));
    for seed in 1..=40u64 {
        let (name, bytes, _) = generated(seed);
        inputs.push((name, bytes));
    }
    for (name, bytes) in inputs {
        let mut p = SseParser::new();
        let mut want: Vec<Result<SseEvent, String>> = p.push(&bytes).into_iter().map(Ok).collect();
        want.extend(p.finish().map(Ok));
        let mut rng = Rng::new(bytes.len() as u64 + 5);
        for round in 0..10 {
            let n = 1 + rng.below(10);
            let cuts = random_cuts(&mut rng, bytes.len(), n);
            assert_eq!(through_the_stream(&bytes, &cuts).await, want, "{name}: round {round} cuts {cuts:?}");
        }
    }
}

#[test]
fn a_stream_of_one_block_per_character_cut_at_every_byte_of_a_multibyte_text() {
    let bytes = "event: m\r\ndata: \u{65e5}\u{672c}\u{1f600}\u{e9}\r\n\r\n".as_bytes();
    let events = assert_split_invariant("multibyte", bytes, true, 0, 1);
    assert_eq!(
        events,
        vec![SseEvent { event: Some("m".into()), data: "\u{65e5}\u{672c}\u{1f600}\u{e9}".into(), id: None }]
    );
}
