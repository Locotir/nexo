//! How long a streamed reply takes to draw, frame by frame.

use super::*;

/// Streams a long Markdown reply in small chunks through the path the screen uses and draws one frame per
/// chunk. Run on its own in release: `nxcargo test -p nexo-tui --release -- --ignored a_long_streamed_reply --nocapture`.
#[test]
#[ignore = "timing run, a minute or so in release"]
fn a_long_streamed_reply_draws_each_frame_in_time() {
    let (mut a, _rx) = bare();
    let reply = crate::render::markdown::stream_tests::long_reply(3000);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    a.handle_agent(AgentEvent::TurnStart { turn: 1 });
    let chars: Vec<char> = reply.chars().collect();
    let mut frames: Vec<std::time::Duration> = Vec::new();
    for chunk in chars.chunks(40) {
        let t = std::time::Instant::now();
        a.handle_agent(AgentEvent::TextDelta { text: chunk.iter().collect() });
        a.tick_frame(false);
        term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
        frames.push(t.elapsed());
    }
    let total: std::time::Duration = frames.iter().sum();
    frames.sort();
    let at = |p: f64| frames[((frames.len() - 1) as f64 * p) as usize];
    println!(
        "streamed {} lines in {} frames: total {:?}, p50 {:?}, p95 {:?}, max {:?}",
        reply.lines().count(),
        frames.len(),
        total,
        at(0.5),
        at(0.95),
        frames[frames.len() - 1]
    );
}
