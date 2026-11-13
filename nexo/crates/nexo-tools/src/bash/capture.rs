use super::*;

pub(super) enum End {
    Exited(i32),
    Signalled,
    TimedOut,
    Cancelled,
    Failed(String),
}

pub(super) struct Outcome {
    pub(super) output: String,
    pub(super) end: End,
}

/// The output as it comes, bounded: the first half of `MAX_OUTPUT_CHARS`
/// is kept whole, then the last half, whatever the command prints in
/// between (a build log of a gigabyte costs no memory).
pub(super) struct Capture {
    head: String,
    tail: VecDeque<String>,
    tail_chars: usize,
    omitted: usize,
}

impl Capture {
    const HALF: usize = MAX_OUTPUT_CHARS / 2;

    pub(super) fn new() -> Capture {
        Capture { head: String::new(), tail: VecDeque::new(), tail_chars: 0, omitted: 0 }
    }

    pub(super) fn push(&mut self, line: &str) {
        let n = line.chars().count() + 1;
        if self.head.len() < Self::HALF {
            self.head.push_str(line);
            self.head.push('\n');
            return;
        }
        self.tail.push_back(line.to_string());
        self.tail_chars += n;
        while self.tail_chars > Self::HALF {
            if let Some(dropped) = self.tail.pop_front() {
                let k = dropped.chars().count() + 1;
                self.tail_chars -= k;
                self.omitted += k;
            }
        }
    }

    /// Names of commands the shell reported as not found, in the lines kept (head and tail,
    /// never the omitted middle): at most 3, each once.
    pub(super) fn not_found(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for line in self.head.lines().chain(self.tail.iter().map(|l| l.as_str())) {
            if let Some(n) = not_found_name(line) {
                if !names.contains(&n) {
                    names.push(n);
                    if names.len() == 3 {
                        break;
                    }
                }
            }
        }
        names
    }

    pub(super) fn text(mut self) -> String {
        if self.omitted > 0 {
            self.head.push_str(&format!("\n… [{} characters omitted] …\n\n", self.omitted));
        }
        for l in self.tail {
            self.head.push_str(&l);
            self.head.push('\n');
        }
        self.head
    }
}
