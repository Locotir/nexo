//! Isolated scenes: one objective, a few requests, no trajectory.
//!
//! `cargo run --release -p nexo-bench --example isolated -- --llmb DIR --instance ID --system-from FILE
//!  [--arm ARM]... [--only OBJ]... [--samples K] [--temperature T] [--upstream URL] [--model M]
//!  [--key-env VAR] [--budget USD] [--out FILE] [--dry]`
//!
//! A scene is the moment a session has already read the material and a new objective arrives: the first
//! message, one turn that reads every material file whole, and the objective next to the results. Every
//! objective of the instance is its own scene, run `K` times: the agent may use the real tools in a fresh
//! workspace holding the material, for at most `--steps` requests (default 6), and its answer is scored by
//! the oracle; a scene that ends without one counts as no answer.
//! Whole runs mix the view with luck (a loop at the first objective sinks the rest); a scene measures one
//! thing: what the agent does with the material in the form an arm gives it.
//!
//! Arms (the form the material takes in the scene):
//! - `raw`: the read results as the read tool prints them.
//! - `reader-obj`: each file goes first through a separate request (the reader) that knows the thread and
//!   the current objective and writes short notes; the scene carries the notes in place of the text.
//! - `reader-map`: as `reader-obj`, but the reader does not know the objective: one set of notes per file,
//!   meant for any later question (reusable across objectives).
//!
//! The scene's system prompt and tools come from the first request of `--system-from` (a transcript of the
//! plain arm). The reader prompt puts the file first and the per-objective part last, so the file is a cache
//! hit from the second objective on. Every call (reader and scene) goes to `--out` as a JSON line, and its
//! cost counts against `--budget`. `--dry` lists the scenes and their size. The key is read from `--key-env`
//! by the provider, never printed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use futures::StreamExt;
use nexo_bench::llmb::{canonstep, read_instance};
use nexo_core::{Collector, Message, ModelRequest, Part, ProviderConfig, ToolContext, ToolOutput, ToolSpec, Usage};
use serde_json::{json, Map, Value};

const ARMS: [&str; 3] = ["raw", "reader-obj", "reader-map"];

struct Args {
    llmb: PathBuf,
    instance: String,
    system_from: PathBuf,
    arms: Vec<String>,
    only: Vec<String>,
    samples: u32,
    temperature: f32,
    upstream: String,
    model: String,
    key_env: String,
    budget: f64,
    out: Option<PathBuf>,
    steps: u32,
    dry: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        llmb: PathBuf::new(),
        instance: String::new(),
        system_from: PathBuf::new(),
        arms: Vec::new(),
        only: Vec::new(),
        samples: 1,
        temperature: 0.0,
        upstream: "https://api.deepseek.com/anthropic".into(),
        model: "deepseek-chat".into(),
        key_env: "DEEPSEEK_API_KEY".into(),
        budget: 0.05,
        out: None,
        steps: 6,
        dry: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or(format!("{k} needs a value"));
        match k.as_str() {
            "--llmb" => a.llmb = v()?.into(),
            "--instance" => a.instance = v()?,
            "--system-from" => a.system_from = v()?.into(),
            "--arm" => a.arms.push(v()?),
            "--only" => a.only.push(v()?),
            "--samples" => a.samples = v()?.parse().map_err(|e| format!("--samples: {e}"))?,
            "--temperature" => a.temperature = v()?.parse().map_err(|e| format!("--temperature: {e}"))?,
            "--upstream" => a.upstream = v()?,
            "--model" => a.model = v()?,
            "--key-env" => a.key_env = v()?,
            "--budget" => a.budget = v()?.parse().map_err(|e| format!("--budget: {e}"))?,
            "--out" => a.out = Some(v()?.into()),
            "--steps" => a.steps = v()?.parse().map_err(|e| format!("--steps: {e}"))?,
            "--dry" => a.dry = true,
            _ => return Err(format!("unknown option {k}")),
        }
    }
    if a.instance.is_empty() || a.system_from.as_os_str().is_empty() {
        return Err("give --llmb DIR, --instance ID and --system-from TRANSCRIPT".into());
    }
    if a.arms.is_empty() {
        a.arms = ARMS.iter().map(|s| s.to_string()).collect();
    }
    if let Some(bad) = a.arms.iter().find(|x| !ARMS.contains(&x.as_str())) {
        return Err(format!("unknown arm {bad} (arms: {})", ARMS.join(", ")));
    }
    Ok(a)
}

/// A file as the read tool prints it whole.
fn numbered(text: &str) -> String {
    text.lines().enumerate().map(|(i, l)| format!("{:>6}│ {l}\n", i + 1)).collect()
}

fn tokens(v: &Value) -> u64 {
    v.to_string().len() as u64 / 4
}

/// The first message of a scene: the material and how to work, without an objective.
fn intro(inst: &canonstep::Adapted) -> String {
    let names: Vec<&str> = inst.material.keys().map(String::as_str).collect();
    format!(
        "Frozen material is in material/: {}. Read as needed. Work only on the current objective; do not solve future cases.",
        names.join(", ")
    )
}

/// The scene: intro, one turn reading every file, and the objective next to what each read gave.
fn scene(inst: &canonstep::Adapted, step: &canonstep::Step, given: &BTreeMap<String, String>) -> Vec<Value> {
    let calls: Vec<Value> = inst
        .material
        .keys()
        .enumerate()
        .map(|(i, f)| json!({"type": "tool_use", "id": format!("read_{i}"), "name": "read", "input": {"path": format!("material/{f}")}}))
        .collect();
    let mut results: Vec<Value> = inst
        .material
        .keys()
        .enumerate()
        .map(|(i, f)| json!({"type": "tool_result", "tool_use_id": format!("read_{i}"), "content": [{"type": "text", "text": given[f]}]}))
        .collect();
    results.push(json!({"type": "text", "text": canonstep::worker_request(inst, step, false)}));
    vec![
        json!({"role": "user", "content": [{"type": "text", "text": intro(inst)}]}),
        json!({"role": "assistant", "content": calls}),
        json!({"role": "user", "content": results}),
    ]
}

const READER_SYSTEM: &str = "You read material for another agent that works on a task. The agent will not see the material, only your notes, so they must hold every fact it needs, stated exactly. Do not guess: when something cannot be settled from the material, say so.";

/// The reader request for `file`: the file first (shared by every objective, so cached), the thread after.
fn reader(inst: &canonstep::Adapted, file: &str, step: Option<&canonstep::Step>) -> Vec<Value> {
    let text = &inst.material[file];
    let lines = text.lines().count();
    let mut s = format!("<material path=\"material/{file}\" lines=\"1-{lines}\">\n{}</material>\n\n", numbered(text));
    s.push_str(&format!("Main thread: {}\n", intro(inst)));
    match step {
        Some(st) => {
            s.push_str(&format!(
                "Current objective of the thread:\n{}\n\n",
                canonstep::worker_request(inst, st, false)
            ));
            s.push_str("Last steps: the agent listed material/ and asked to read this file whole.\n");
            s.push_str("Why it reads it: to answer the current objective.\n\n");
            s.push_str(
                "Answer format: at most 12 lines `Finding: ...` with the facts from this material that the objective needs, \
                 each citing its line numbers, then one line `Conclusion: ...` with the result they imply, or \
                 `Conclusion: unknown`. If this material is not about the objective, answer only `Finding: not about this objective.`",
            );
        }
        None => {
            s.push_str("Last steps: the agent listed material/ and asked to read this file whole.\n");
            s.push_str("Why it reads it: later objectives will ask about it; which ones is not known yet.\n\n");
            s.push_str(
                "Answer format: at most 25 lines `Finding: ...`: what this material is and, for each part, what it does, \
                 precise enough to answer any question about it without the original (for code: its exact behaviour as \
                 short pseudocode, edge cases included), each citing its line numbers.",
            );
        }
    }
    vec![json!({"role": "user", "content": [{"type": "text", "text": s}]})]
}

/// A tool output cut to what one request may carry.
fn cut(s: &str) -> String {
    const MAX: usize = 30_000;
    if s.len() <= MAX {
        return s.to_string();
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output cut at {MAX} bytes]", &s[..end])
}

/// What the scene shows in place of a file the reader went through.
fn notes_result(inst: &canonstep::Adapted, file: &str, notes: &str) -> String {
    let lines = inst.material[file].lines().count();
    format!(
        "[curator] material/{file} ({lines} lines, unchanged) was read whole by a separate pass for this thread; its notes follow. The full text can be read again.\n{}",
        notes.trim()
    )
}

/// Whether `text` answers `obj` right, by the oracle.
fn right(a: &canonstep::Adapted, expected: &Map<String, Value>, obj: &str, text: &str) -> bool {
    let Some(st) = canonstep::statements(text, obj, &a.steps) else { return false };
    let mut stated: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (key, comps) in st.into_iter().filter(|(k, _)| k == obj) {
        stated.entry(key).or_default().extend(comps);
    }
    let snap = canonstep::score_snapshot(a, &stated, expected);
    snap.cases.iter().any(|(id, s)| id == obj && s.total > 0 && s.correct == s.total)
}

struct Caller {
    provider: ProviderConfig,
    client: nexo_providers::Client,
    model: String,
    temperature: f32,
    budget: f64,
    spent: f64,
    log: Option<std::fs::File>,
}

struct Reply {
    msg: Message,
    text: String,
    calls: usize,
    usage: Usage,
    cost: f64,
}

impl Caller {
    /// One line in the log that is not a call (a scene's verdict).
    fn note(&mut self, rec: Value) -> Result<(), String> {
        use std::io::Write;
        match &mut self.log {
            Some(f) => writeln!(f, "{rec}").map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    async fn call(
        &mut self,
        system: &str,
        messages: &[Value],
        tools: Vec<ToolSpec>,
        tag: Value,
    ) -> Result<Reply, String> {
        if self.spent >= self.budget {
            return Err(format!("budget reached at {:.4} $", self.spent));
        }
        let msgs: Vec<Message> = messages
            .iter()
            .map(|m| serde_json::from_value(m.clone()))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let mut req = ModelRequest::new(&self.model, system, msgs);
        req.tools = tools;
        req.max_tokens = 4096;
        req.temperature = Some(self.temperature);
        let mut stream = self.client.stream(&self.provider, &req, 2).await.map_err(|e| e.to_string())?;
        let mut col = Collector::new();
        while let Some(ev) = stream.next().await {
            col.push(ev);
        }
        if let Some(e) = &col.error {
            return Err(format!("the reply failed: {e}"));
        }
        let cost = nexo_bench::cost::cost(&nexo_bench::cost::price_of(&self.model).unwrap_or_default(), &col.usage);
        self.spent += cost;
        let msg = col.message();
        let calls = msg.content.iter().filter(|p| matches!(p, Part::ToolCall { .. })).count();
        let text: String = msg
            .content
            .iter()
            .filter_map(|p| if let Part::Text { text } = p { Some(text.as_str()) } else { None })
            .collect();
        if let Some(f) = &mut self.log {
            use std::io::Write;
            let mut rec = tag;
            rec["text"] = json!(text);
            let shown: Vec<String> = msg
                .tool_calls()
                .map(|(_, n, a)| format!("{n} {}", a.to_string().chars().take(300).collect::<String>()))
                .collect();
            rec["calls"] = json!(shown);
            rec["input"] = json!(col.usage.input);
            rec["cache_read"] = json!(col.usage.cache_read);
            rec["output"] = json!(col.usage.output);
            rec["cost"] = json!(cost);
            writeln!(f, "{rec}").map_err(|e| e.to_string())?;
        }
        Ok(Reply { msg, text, calls, usage: col.usage, cost })
    }
}

#[derive(Default, Clone)]
struct Tally {
    /// Right answers per sample.
    right: Vec<u64>,
    /// Scenes that ran out of steps without an answer.
    open: u64,
    wrong: u64,
    /// Requests sent in scenes.
    requests: u64,
    scene_input: u64,
    reader_cost: f64,
    scene_cost: f64,
    /// Per objective: right count over the samples.
    per: BTreeMap<String, u32>,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("isolated: {e}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let a = args()?;
    let battery = canonstep::battery(&a.llmb)?;
    let inst = battery.iter().find(|x| x.id == a.instance).ok_or(format!("no instance {}", a.instance))?;
    let oracle = read_instance(&a.llmb.join(&inst.path))?.oracle()?;
    let first = std::fs::read_to_string(&a.system_from).map_err(|e| format!("{}: {e}", a.system_from.display()))?;
    let rec: Value = serde_json::from_str(first.lines().next().unwrap_or("")).map_err(|e| e.to_string())?;
    let system = rec.get("system").and_then(Value::as_str).ok_or("no system in --system-from")?.to_string();
    let tool_names: Vec<String> = rec
        .get("tools")
        .and_then(Value::as_array)
        .map(|t| t.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let sets = [nexo_tools::builtin(), nexo_tools::local()];
    let mut specs: BTreeMap<String, ToolSpec> = BTreeMap::new();
    for set in &sets {
        for s in set.specs() {
            specs.entry(s.name.clone()).or_insert(s);
        }
    }
    let tools: Vec<ToolSpec> = tool_names.iter().filter_map(|t| specs.get(t).cloned()).collect();
    let steps: Vec<&canonstep::Step> =
        inst.steps.iter().filter(|s| a.only.is_empty() || a.only.contains(&s.id)).collect();
    let raw: BTreeMap<String, String> = inst.material.iter().map(|(f, t)| (f.clone(), numbered(t))).collect();

    let probe = scene(inst, steps[0], &raw);
    println!(
        "{} scenes x {} arms x {} samples; raw scene about {} tokens, system {} tokens, {} tools",
        steps.len(),
        a.arms.len(),
        a.samples,
        probe.iter().map(tokens).sum::<u64>(),
        system.len() / 4,
        tools.len()
    );
    for f in inst.material.keys() {
        println!(
            "reader request for {f}: about {} tokens",
            reader(inst, f, Some(steps[0])).iter().map(tokens).sum::<u64>()
        );
    }
    if a.dry {
        return Ok(());
    }

    let log = match &a.out {
        Some(p) => Some(std::fs::OpenOptions::new().create(true).append(true).open(p).map_err(|e| e.to_string())?),
        None => None,
    };
    let mut c = Caller {
        provider: ProviderConfig::new(nexo_bench::protocol_of(&a.upstream), &a.upstream, Some(a.key_env.as_str())),
        client: nexo_providers::Client::new(nexo_providers::registry()),
        model: a.model.clone(),
        temperature: a.temperature,
        budget: a.budget,
        spent: 0.0,
        log,
    };
    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    let res = async {
        for k in 0..a.samples {
            for arm in &a.arms {
                let t = tally.entry(arm.clone()).or_default();
                t.right.push(0);
                // the notes that do not depend on the objective: once per file and sample
                let mut map: BTreeMap<String, String> = BTreeMap::new();
                if arm == "reader-map" {
                    for f in inst.material.keys() {
                        let tag = json!({"arm": arm, "sample": k, "role": "reader", "file": f});
                        let r = c.call(READER_SYSTEM, &reader(inst, f, None), Vec::new(), tag).await?;
                        t.reader_cost += r.cost;
                        map.insert(f.clone(), notes_result(inst, f, &r.text));
                    }
                }
                for st in &steps {
                    let given = match arm.as_str() {
                        "raw" => raw.clone(),
                        "reader-map" => map.clone(),
                        _ => {
                            let mut g = BTreeMap::new();
                            for f in inst.material.keys() {
                                let tag =
                                    json!({"arm": arm, "sample": k, "role": "reader", "file": f, "objective": st.id});
                                let r = c.call(READER_SYSTEM, &reader(inst, f, Some(st)), Vec::new(), tag).await?;
                                t.reader_cost += r.cost;
                                g.insert(f.clone(), notes_result(inst, f, &r.text));
                            }
                            g
                        }
                    };
                    let mut msgs = scene(inst, st, &given);
                    let ws =
                        std::env::temp_dir().join(format!("nexo-isolated-{}-{arm}-{k}-{}", std::process::id(), st.id));
                    std::fs::create_dir_all(ws.join("material")).map_err(|e| e.to_string())?;
                    for (f, text) in &inst.material {
                        std::fs::write(ws.join("material").join(f), text).map_err(|e| e.to_string())?;
                    }
                    let ctx = ToolContext::new(ws.clone());
                    let mut verdict = "open";
                    for s in 0..a.steps {
                        let tag = json!({"arm": arm, "sample": k, "role": "scene", "objective": st.id, "step": s});
                        let r = c.call(&system, &msgs, tools.clone(), tag).await?;
                        t.scene_cost += r.cost;
                        t.scene_input += r.usage.input + r.usage.cache_read;
                        t.requests += 1;
                        if r.calls == 0 {
                            verdict = if right(inst, &oracle.expected, &st.id, &r.text) { "right" } else { "wrong" };
                            break;
                        }
                        let mut results = Vec::new();
                        for (id, name, args) in r.msg.tool_calls() {
                            let out = match sets.iter().find_map(|set| set.get(name)) {
                                Some(tool) => tool.run(args.clone(), &ctx).await,
                                None => ToolOutput::error(format!("unknown tool {name}")),
                            };
                            results.push(Part::tool_result(id, cut(&out.content), out.is_error));
                        }
                        msgs.push(serde_json::to_value(&r.msg).map_err(|e| e.to_string())?);
                        msgs.push(serde_json::to_value(Message::tool_results(results)).map_err(|e| e.to_string())?);
                    }
                    let _ = std::fs::remove_dir_all(&ws);
                    c.note(
                        json!({"arm": arm, "sample": k, "role": "verdict", "objective": st.id, "verdict": verdict}),
                    )?;
                    match verdict {
                        "right" => {
                            *t.right.last_mut().unwrap() += 1;
                            *t.per.entry(st.id.clone()).or_default() += 1;
                        }
                        "wrong" => t.wrong += 1,
                        _ => t.open += 1,
                    }
                    println!("{arm} #{k} {}: {verdict}", st.id);
                }
            }
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = res {
        eprintln!("stopped: {e}");
    }

    println!("\n| arm | right per sample | mean | wrong | no answer | requests per scene | scene input tokens | reader $ | scene $ | $ per right |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for (arm, t) in &tally {
        let n = t.right.len().max(1) as f64;
        let mean = t.right.iter().sum::<u64>() as f64 / n;
        let per: Vec<String> = t.right.iter().map(u64::to_string).collect();
        println!(
            "| {arm} | {} | {mean:.2} / {} | {} | {} | {:.1} | {} | {:.4} | {:.4} | {:.4} |",
            per.join(" "),
            steps.len(),
            t.wrong,
            t.open,
            t.requests as f64 / (n * steps.len() as f64),
            t.scene_input,
            t.reader_cost,
            t.scene_cost,
            (t.reader_cost + t.scene_cost) / (mean * n).max(1.0)
        );
    }
    println!("\n| objective | {} |", tally.keys().cloned().collect::<Vec<_>>().join(" | "));
    println!("|---|{}", "---|".repeat(tally.len()));
    for st in &steps {
        let row: Vec<String> =
            tally.values().map(|t| format!("{}/{}", t.per.get(&st.id).unwrap_or(&0), t.right.len())).collect();
        println!("| {} | {} |", st.id, row.join(" | "));
    }
    println!("\nspent {:.4} $", c.spent);
    Ok(())
}
