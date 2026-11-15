//! The scanner behind [`super::segments`]: the shared shell lexer's commands, each turned into a [`Seg`] with its
//! input, output, folder and heredoc.

use std::path::{Path, PathBuf};

use super::program;
use super::words::{is_assignment, shell_script, strip_wrappers};
use super::{Input, Seg, BODY_CAP, MAX_DEPTH};

pub(super) struct Scan {
    src: String,
    out: Vec<Seg>,
    dir: PathBuf,
    stack: Vec<PathBuf>,
    depth: u8,
}

impl Scan {
    pub(super) fn new(command: &str, dir: &Path, depth: u8) -> Scan {
        Scan { src: command.to_string(), out: Vec::new(), dir: dir.to_path_buf(), stack: Vec::new(), depth }
    }

    pub(super) fn run(mut self) -> Option<Vec<Seg>> {
        use nexo_core::shell::{lex, End};
        let lx = lex(&self.src, true);
        if lx.open_quote || !lx.substs.is_empty() || lx.bad_heredoc {
            return None;
        }
        for c in &lx.cmds {
            let mut seg = Seg::default();
            let lead = c.words.iter().take_while(|w| is_assignment(w)).count();
            seg.words = c.words[lead..].to_vec();
            for r in &c.redirects {
                let Some(t) = &r.target else { continue };
                if r.dup {
                    continue;
                }
                match r.op.as_str() {
                    "<" => seg.input = Some(Input::File(t.clone())),
                    "<<<" => seg.input = Some(Input::Text(t.clone())),
                    "<&" => {}
                    _ => {
                        let other = r.fd.as_deref().is_some_and(|f| f != "1") && r.op.starts_with('>');
                        if !other {
                            seg.output = Some(t.clone());
                        }
                    }
                }
            }
            if let Some(h) = &c.heredoc {
                match &h.body {
                    Some(b) => {
                        let mut o = String::new();
                        for l in b.split_inclusive('\n') {
                            if o.len() < BODY_CAP {
                                o.push_str(l);
                            }
                        }
                        seg.input = Some(Input::Text(o));
                    }
                    None => seg.pending = Some((h.delim.clone(), h.strip)),
                }
            }
            self.finish(seg);
            match c.end {
                End::Open => self.stack.push(self.dir.clone()),
                End::Close => {
                    if let Some(d) = self.stack.pop() {
                        self.dir = d;
                    }
                }
                _ => {}
            }
        }
        self.out.retain(|s| !s.words.is_empty());
        Some(self.out)
    }

    fn finish(&mut self, mut seg: Seg) {
        if seg.words.is_empty() && seg.pending.is_none() {
            return;
        }
        seg.words.retain(|w| w != "{" && w != "}");
        seg.words = strip_wrappers(seg.words);
        seg.dir = self.dir.clone();
        let waits = seg.pending.is_some();
        match seg.words.first().map(|w| program(w)).as_deref() {
            Some("cd") => {
                // `cd`, `cd -` and `cd ~`/`cd $X` say nothing about a folder: what follows is searched where it was
                if let Some(t) = seg.words.get(1).filter(|t| !t.starts_with(['-', '~']) && !t.contains('$')) {
                    self.dir = self.dir.join(t);
                }
                if !waits {
                    return;
                }
            }
            Some("bash" | "sh" | "zsh" | "dash" | "ksh") => {
                if let Some(script) = shell_script(&seg.words).filter(|_| self.depth < MAX_DEPTH) {
                    if let Some(inner) = Scan::new(&script, &self.dir, self.depth + 1).run() {
                        self.out.extend(inner);
                    }
                    if !waits {
                        return;
                    }
                }
            }
            _ => {}
        }
        self.out.push(seg);
    }
}
