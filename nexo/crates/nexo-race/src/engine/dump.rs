//! Measurement: the features of the units the view holds, written to a file.

use crate::hindsight::UKind;
use crate::learn::MODEL;

use super::Engine;

/// Where `Engine::dump_to` writes, how often (every `stride` requests) and how many requests it saw. The file is
/// shared with the copy of the engine a landing is tried on (`Engine::decide`): whichever goes on writes to it.
#[derive(Clone)]
pub(super) struct Dump {
    out: std::sync::Arc<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>,
    stride: usize,
    count: usize,
}

impl Engine {
    /// For measurement: writes to `path`, every `stride` requests, the features of each unit the view
    /// holds and the shipped heads' guesses for it, and at the end what each unit came to be used by.
    pub fn dump_to(&mut self, path: &str, stride: usize) -> std::io::Result<()> {
        let out = std::sync::Arc::new(std::sync::Mutex::new(std::io::BufWriter::new(std::fs::File::create(path)?)));
        self.dump = Some(Dump { out, stride: stride.max(1), count: 0 });
        Ok(())
    }

    /// Ends the dump: the uses of the units so far, then the file is closed.
    pub fn dump_end(&mut self) {
        self.dump_uses();
        self.dump = None;
    }

    pub(super) fn dump_rows(&mut self) {
        use std::io::Write;
        let Some(d) = self.dump.as_mut() else { return };
        d.count += 1;
        if (d.count - 1) % d.stride != 0 {
            return;
        }
        let t = self.tr.msgs.len();
        self.need_context();
        let vis = self.visible();
        let Some(d) = self.dump.as_ref() else { return };
        let Ok(mut out) = d.out.lock() else { return };
        let mut line = String::new();
        for &mi in vis.iter() {
            for &i in self.fz.by_msg(mi) {
                let u = &self.tr.units[i];
                let gone = self.dropped.get(&i);
                if u.tokens < 8 || gone.is_some_and(|g| g.len() == self.fz.chunks[i].len()) {
                    continue;
                }
                let (mut pt, mut pk) = (0.0f64, 0.0f64);
                if u.kind == UKind::Result {
                    for j in 0..self.fz.chunks[i].len() {
                        if !gone.is_some_and(|g| g.contains(&j)) {
                            let x = self.fz.vector(&self.tr, i, j, &self.active);
                            pt = pt.max(MODEL.turn_prob(&x));
                            pk = pk.max(MODEL.prob(&x));
                        }
                    }
                } else {
                    let x = self.fz.vector(&self.tr, i, 0, &self.active);
                    pt = MODEL.turn_prob(&x);
                    pk = MODEL.prob(&x);
                }
                let x = self.fz.unit_vector(&self.tr, i, &self.active);
                line.clear();
                line.push_str(&format!("x\t{t}\t{i}\t{mi}\t{}\t{pt:.5}\t{pk:.5}", u.tokens));
                for v in x {
                    line.push_str(&format!("\t{v:.4}"));
                }
                line.push('\n');
                let _ = out.write_all(line.as_bytes());
            }
        }
    }

    pub(super) fn dump_uses(&mut self) {
        use std::io::Write;
        let Some(d) = self.dump.as_ref() else { return };
        let Ok(mut out) = d.out.lock() else { return };
        let mut line = String::new();
        for (i, u) in self.tr.units.iter().enumerate() {
            let kind = match u.kind {
                UKind::User => 'U',
                UKind::Result => 'R',
                UKind::Text => 'T',
                UKind::Thinking => 'K',
                UKind::Call => 'C',
            };
            let uses: Vec<String> = u.uses.iter().map(|m| m.to_string()).collect();
            line.clear();
            line.push_str(&format!("u\t{i}\t{}\t{kind}\t{}\t{}\n", u.msg, u.tokens, uses.join(",")));
            let _ = out.write_all(line.as_bytes());
        }
        let steps: Vec<String> = self.tr.steps.iter().map(|m| m.to_string()).collect();
        let _ = out.write_all(format!("s\t{}\ne\n", steps.join(",")).as_bytes());
        let _ = out.flush();
    }
}
