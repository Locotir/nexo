use super::*;

pub(super) enum End {
    Exited(i32),
    /// Killed by this signal (0 where the system does not say which).
    Signalled(i32),
    TimedOut,
    Cancelled,
    /// The command could not be started.
    Failed(String),
    /// The command started, but waiting for it failed: how it ended is not known.
    Lost(String),
}

/// The name of a signal that commonly ends a command; empty for any other number.
fn signal_name(n: i32) -> &'static str {
    #[cfg(unix)]
    {
        match n {
            libc::SIGHUP => "SIGHUP",
            libc::SIGINT => "SIGINT",
            libc::SIGQUIT => "SIGQUIT",
            libc::SIGILL => "SIGILL",
            libc::SIGABRT => "SIGABRT",
            libc::SIGBUS => "SIGBUS",
            libc::SIGFPE => "SIGFPE",
            libc::SIGKILL => "SIGKILL",
            libc::SIGSEGV => "SIGSEGV",
            libc::SIGPIPE => "SIGPIPE",
            libc::SIGALRM => "SIGALRM",
            libc::SIGTERM => "SIGTERM",
            libc::SIGXCPU => "SIGXCPU",
            libc::SIGXFSZ => "SIGXFSZ",
            _ => "",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = n;
        ""
    }
}

/// `killed by a signal: 9 SIGKILL`, `killed by a signal: 64` for a number with no name, and the
/// bare words where the system gave no number.
pub(super) fn signal_words(n: i32) -> String {
    match (n, signal_name(n)) {
        (0, _) => "killed by a signal".into(),
        (n, "") => format!("killed by a signal: {n}"),
        (n, name) => format!("killed by a signal: {n} {name}"),
    }
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
    head_chars: usize,
    tail: VecDeque<String>,
    tail_chars: usize,
    omitted: usize,
    highlights: String,
    highlight_dropped: usize,
    context_left: usize,
    line_cut: bool,
}

impl Capture {
    const HALF: usize = MAX_OUTPUT_CHARS / 2;

    pub(super) fn new() -> Capture {
        Capture {
            head: String::new(),
            head_chars: 0,
            tail: VecDeque::new(),
            tail_chars: 0,
            omitted: 0,
            highlights: String::new(),
            highlight_dropped: 0,
            context_left: 0,
            line_cut: false,
        }
    }

    pub(super) fn push(&mut self, line: &str) {
        self.line_cut |= line.contains(" bytes of this line cut]");
        let trimmed = line.trim_start();
        let important = trimmed.starts_with("error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("failures:")
            || trimmed.starts_with("test result:")
            || trimmed.starts_with("thread '") && trimmed.contains("panicked")
            || trimmed.starts_with("FAILED")
            || trimmed.starts_with("test ") && trimmed.contains(" ... FAILED")
            || trimmed.starts_with("could not compile");
        if important {
            self.context_left = 3;
        }
        if self.context_left > 0 {
            let excerpt: String = line.chars().take(500).collect();
            if self.highlights.len() + excerpt.len() < 6000 {
                self.highlights.push_str(&excerpt);
                self.highlights.push('\n');
            } else {
                self.highlight_dropped += 1;
            }
            self.context_left -= 1;
        }
        let n = line.chars().count() + 1;
        // `HALF` counts characters, like the tail: a byte count would hand non-ASCII output to the tail early
        if self.head_chars < Self::HALF {
            self.head.push_str(line);
            self.head.push('\n');
            self.head_chars += n;
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

    pub(super) fn truncated(&self) -> bool {
        self.omitted > 0 || self.line_cut
    }

    pub(super) fn highlights(&self) -> String {
        if !self.truncated() || self.highlights.is_empty() {
            return String::new();
        }
        format!(
            "\n\n[selected diagnostic lines, possibly incomplete; {} lines omitted from selection]\n{}",
            self.highlight_dropped, self.highlights
        )
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
