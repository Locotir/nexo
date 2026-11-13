//! The leading `<think>` block that some servers send inside `content`.

/// Where a reply stands with respect to a leading `<think>` block.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum ThinkState {
    /// Nothing but blanks has come: the reply may still open with the tag.
    #[default]
    Start,
    Thinking,
    /// The block just closed: the blanks that follow it are not the answer.
    Closed,
    Answer,
}

/// Splits a leading `<think>...</think>` out of the text of a reply: servers that do not separate
/// reasoning themselves (LM Studio, llama.cpp, vLLM and Ollama without their reasoning option) send
/// it inside `content`. A tag cut between two chunks is held until it is whole.
#[derive(Debug, Default)]
pub(super) struct ThinkSplit {
    state: ThinkState,
    held: String,
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

impl ThinkSplit {
    /// The pieces to emit, in order: (is reasoning, text).
    pub(super) fn push(&mut self, s: &str) -> Vec<(bool, String)> {
        let mut out = Vec::new();
        let mut buf = std::mem::take(&mut self.held);
        buf.push_str(s);
        loop {
            match self.state {
                ThinkState::Start => {
                    let t = buf.trim_start();
                    if t.is_empty() || THINK_OPEN.starts_with(t) {
                        self.held = buf;
                        return out;
                    }
                    match t.strip_prefix(THINK_OPEN) {
                        Some(rest) => {
                            self.state = ThinkState::Thinking;
                            buf = rest.to_string();
                        }
                        None => self.state = ThinkState::Answer,
                    }
                }
                ThinkState::Thinking => match buf.find(THINK_CLOSE) {
                    Some(i) => {
                        if i > 0 {
                            out.push((true, buf[..i].to_string()));
                        }
                        buf = buf[i + THINK_CLOSE.len()..].to_string();
                        self.state = ThinkState::Closed;
                    }
                    None => {
                        let keep = (1..THINK_CLOSE.len())
                            .rev()
                            .find(|k| {
                                buf.len() >= *k
                                    && buf.is_char_boundary(buf.len() - k)
                                    && THINK_CLOSE.starts_with(&buf[buf.len() - k..])
                            })
                            .unwrap_or(0);
                        let cut = buf.len() - keep;
                        if cut > 0 {
                            out.push((true, buf[..cut].to_string()));
                        }
                        self.held = buf[cut..].to_string();
                        return out;
                    }
                },
                ThinkState::Closed => {
                    let t = buf.trim_start();
                    if t.is_empty() {
                        return out;
                    }
                    buf = t.to_string();
                    self.state = ThinkState::Answer;
                }
                ThinkState::Answer => {
                    if !buf.is_empty() {
                        out.push((false, buf));
                    }
                    return out;
                }
            }
        }
    }

    /// The reply ended: what is held goes out as what it was.
    pub(super) fn flush(&mut self) -> Vec<(bool, String)> {
        let held = std::mem::take(&mut self.held);
        let thinking = self.state == ThinkState::Thinking;
        self.state = ThinkState::Answer;
        if held.trim().is_empty() {
            Vec::new()
        } else {
            vec![(thinking, held)]
        }
    }
}
