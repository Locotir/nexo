//! What of the engine a saved state (`crate::snapshot`) leaves out and takes back from the history.

use serde_json::Value;

use crate::json::strip_cc;

use super::Engine;

impl Engine {
    /// The messages taken in so far.
    pub(crate) fn covered(&self) -> usize {
        self.tr.msgs.len()
    }

    /// The messages shortened when they came (`own.born`), as they became, in JSON: the rest of the engine's copy
    /// of the history is the history as it came, and is taken from it again. Shortening them again would take
    /// most of a restore's time, and what they became is less than they were.
    pub(crate) fn born_messages(&self) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> =
            self.own.born.iter().filter_map(|&i| Some((i, self.tr.msgs.get(i)?.to_string()))).collect();
        out.sort_unstable();
        out
    }

    /// Takes back the copy of the history a saved state leaves out: `msgs` are the messages it covered, `born`
    /// what `born_messages` gave.
    pub(crate) fn take_history(&mut self, msgs: &[Value], born: &[(usize, String)]) -> Result<(), String> {
        if self.sizes.len() != msgs.len() || self.fz.by_msg.len() > msgs.len() {
            return Err(format!("the engine holds {} messages, the state says {}", self.sizes.len(), msgs.len()));
        }
        if born.len() != self.own.born.len() || born.iter().any(|(i, _)| *i >= msgs.len() || !self.own.born.contains(i))
        {
            return Err("the shortened messages differ".into());
        }
        let mut out: Vec<Value> = msgs.iter().map(strip_cc).collect();
        for (i, text) in born {
            out[*i] = serde_json::from_str(text).map_err(|e| format!("message {i}: {e}"))?;
        }
        self.last_raw = msgs.last().map(strip_cc).unwrap_or(Value::Null);
        self.tr.mt().msgs = out;
        // the edits and their ids are in the state; the caches built from them are not
        super::edit_index::sync_edit_caches(&mut self.edit_ids, &mut self.edit_index, &self.edits);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curator::Curator;
    use crate::snapshot::options;
    use serde::de::DeserializeOwned;
    use serde::Serialize;

    fn enc<T: Serialize>(x: &T) -> Vec<u8> {
        options().serialize(x).unwrap()
    }

    fn dec<T: DeserializeOwned>(b: &[u8]) -> T {
        options().deserialize(b).unwrap()
    }

    /// One request of a recorded session: what is sent, when, and the messages it is over.
    type Req = (Value, f64, Vec<Value>);

    /// The requests of a recorded session (`tests/fixtures`).
    fn requests(name: &str) -> Vec<Req> {
        let text = std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let mut msgs: Vec<Value> = Vec::new();
        let mut out = Vec::new();
        for line in text.lines() {
            let rec: Value = serde_json::from_str(line).unwrap();
            let req = &rec["req"];
            let send = if let Some(new) = req.get("new") {
                msgs.truncate(req["base"].as_u64().unwrap() as usize);
                msgs.extend(new.as_array().unwrap().iter().cloned());
                serde_json::json!({"op": "view", "system_tokens": req.get("system_tokens").cloned().unwrap_or(Value::Null), "messages": msgs})
            } else {
                req.clone()
            };
            out.push((send, rec["now"].as_f64().unwrap_or(0.0), msgs.clone()));
        }
        out
    }

    /// The curator of settings `spec` that ran recorded session `name` to its middle, and the requests.
    fn at_middle(name: &str, spec: &str) -> (Curator, Vec<Req>, usize) {
        let reqs = requests(name);
        let mut cur = Curator::new(spec).unwrap();
        let half = (reqs.len() / 2..reqs.len()).find(|&k| reqs[k].0["op"] == "view").unwrap();
        for (send, now, _) in &reqs[..=half] {
            cur.answer(send, *now);
        }
        (cur, reqs, half)
    }

    /// Runs a recorded session to its middle, copies the curator, changes the copy's engine (`change`) and takes
    /// its history back as a restore does, then runs the rest of the session on both: whether any reply differs.
    fn replies_differ(name: &str, spec: &str, change: impl Fn(&mut Engine)) -> bool {
        let (mut cur, reqs, half) = at_middle(name, spec);
        let mut back = cur.clone();
        let born = back.eng.born_messages();
        change(&mut back.eng);
        back.eng.take_history(&reqs[half].2[..cur.covered()], &born).unwrap();
        reqs[half + 1..].iter().any(|(send, now, _)| back.answer(send, *now) != cur.answer(send, *now))
    }

    /// A long recorded session with the cache pockets on, so that every part holds something at its middle.
    const SESSION: &str = "bench-marathon-D.jsonl";
    const SPEC: &str = "pockets=1";

    /// One part of the engine written in the saved format and read back in place of the one that ran: the same
    /// bytes come out again (compared byte by byte), and the rest of the session gets the same replies. A part the session left empty
    /// would prove nothing, so each holds something at that point.
    macro_rules! part {
        ($test:ident, $($field:ident).+) => {
            #[test]
            fn $test() {
                assert!(!replies_differ(SESSION, SPEC, |e| {
                    let b = enc(&e.$($field).+);
                    assert!(b.len() > 2, "{} is empty at this point of the session", stringify!($($field).+));
                    e.$($field).+ = dec(&b);
                    // the bytes, not their number: a field that reads back reordered or changed is another state
                    assert!(enc(&e.$($field).+) == b, "{} reads back as other bytes", stringify!($($field).+));
                }));
            }
        };
    }

    part!(the_message_trace_reads_back, tr);
    part!(the_featurizer_reads_back, fz);
    part!(the_request_threads_read_back, goal);
    part!(the_line_index_reads_back, index);
    part!(the_word_interner_reads_back, int);
    part!(the_record_of_what_left_the_view_reads_back, own);
    part!(the_sticky_set_reads_back, stk);
    part!(the_learned_rules_read_back, rules);
    part!(the_standing_facts_read_back, standing);
    part!(the_ledger_names_read_back, names);
    part!(the_cells_read_back, cells);
    part!(the_files_of_each_step_read_back, sf);
    part!(the_lead_index_reads_back, lead_ix);
    part!(the_outputs_below_the_view_read_back, below);
    part!(the_built_messages_read_back, built);
    part!(the_cache_pockets_read_back, pockets);
    part!(the_cache_probe_reads_back, probe);
    part!(the_settings_read_back, p);
    part!(the_steps_read_back, steps);
    part!(the_repeated_runs_read_back, dups);
    part!(the_writes_read_back, wrote);
    part!(the_test_runs_read_back, runs_at);

    /// The check above can fail: an engine whose featurizer starts over answers otherwise.
    #[test]
    fn an_engine_that_lost_a_part_answers_otherwise() {
        assert!(replies_differ(SESSION, SPEC, |e| e.fz = Default::default()));
    }
}
