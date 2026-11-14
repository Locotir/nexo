//! Offline replay of recorded runs against the one-step-work results: the recorded `edit` and `write`
//! calls are run with the real tools on the task's own files in a temp folder, and each later look at a
//! file the model changed is checked against what the new results would have told it. No model, no network.
//! Runs only when `NEXO_Q2_LOGS` lists step logs (a path per line); prints one line per log.

use nexo_core::{Tool, ToolContext};
use serde_json::{json, Value};

fn norm(p: &str) -> String {
    let p = p.trim_start_matches("./");
    match p.strip_prefix("/tmp/nexo-bench-") {
        Some(rest) => rest.split_once('/').map(|x| x.1).unwrap_or("").to_string(),
        None => p.to_string(),
    }
}

/// `cat F`, `sed -n 'A,Bp' F`, `head [-n N] F`, `tail [-n N] F` at the end of a command.
fn printed_path(cmd: &str) -> Option<String> {
    let last = cmd.rsplit([';', '&']).next()?.trim();
    let w: Vec<&str> = last.split_whitespace().collect();
    let ok = matches!(w.first(), Some(&"cat" | &"sed" | &"head" | &"tail")) && w.len() >= 2 && w.len() <= 5;
    let f = *w.last()?;
    (ok && !f.starts_with('-') && !f.contains(['|', '>', '<', '$', '*'])).then(|| norm(f.trim_matches(['\'', '"'])))
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_recorded_runs() {
    let Ok(list) = std::env::var("NEXO_Q2_LOGS") else { return };
    for log in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let name = std::path::Path::new(log).file_name().unwrap().to_string_lossy().into_owned();
        let task_id = name.split("-plain-").next().unwrap_or("").to_string();
        let Some(task) = nexo_bench::tasks::all().into_iter().find(|t| t.id == task_id) else {
            println!("{name}: no task {task_id}");
            continue;
        };
        let wd = test_tmpdir().unwrap();
        task.materialize(wd.path()).unwrap();
        let text = std::fs::read_to_string(log).unwrap();
        let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        let mut msgs: Vec<Value> = last["messages"].as_array().unwrap().clone();
        msgs.push(json!({"role": "assistant", "content": last["reply"]["content"]}));
        let ctx = ToolContext::new(wd.path().to_path_buf());
        // path -> how the model last saw it: Some(true) whole in a result, Some(false) not whole, None unknown
        let mut shown: std::collections::HashMap<String, bool> = Default::default();
        let (mut replayed, mut failed, mut whole, mut backs, mut answered, mut data_lines) = (0, 0, 0, 0, 0, 0);
        let mut csv_facts: Vec<String> = Vec::new();
        for m in &msgs {
            let Some(parts) = m["content"].as_array().filter(|_| m["role"] == "assistant") else { continue };
            for c in parts.iter().filter(|c| c["type"] == "tool_use") {
                let (tool, input) = (c["name"].as_str().unwrap_or(""), &c["input"]);
                match tool {
                    "edit" | "write" => {
                        let mut input = input.clone();
                        // recorded paths point into the run's own folder
                        if let Some(p) = input["path"].as_str() {
                            input["path"] = json!(norm(p));
                        }
                        if let Some(fs) = input["files"].as_array_mut() {
                            for f in fs {
                                f["path"] = json!(norm(f["path"].as_str().unwrap_or("")));
                            }
                        }
                        input.as_object_mut().unwrap().remove("check");
                        let out = if tool == "edit" {
                            nexo_tools::edit::Edit.run(input.clone(), &ctx).await
                        } else {
                            let p = wd.path().join(input["path"].as_str().unwrap_or(""));
                            if p.exists() {
                                ctx.seen.mark(&p);
                            }
                            nexo_tools::write::Write.run(input.clone(), &ctx).await
                        };
                        if out.is_error {
                            failed += 1;
                            continue;
                        }
                        replayed += 1;
                        let paths: Vec<String> = match input["files"].as_array() {
                            Some(fs) => fs.iter().filter_map(|f| f["path"].as_str().map(String::from)).collect(),
                            None => vec![input["path"].as_str().unwrap_or("").to_string()],
                        };
                        for p in paths {
                            let body = std::fs::read_to_string(wd.path().join(&p)).unwrap_or_default();
                            let is_whole = nexo_tools::edit::whole(&body).is_some();
                            whole += is_whole as u32;
                            shown.insert(p.clone(), is_whole);
                            if nexo_tools::data_check::is_data(&wd.path().join(&p)) {
                                csv_facts.push(nexo_tools::data_check::report(&wd.path().join(&p)).trim().to_string());
                                data_lines += 1;
                            }
                        }
                    }
                    "bash" => {
                        let cmd = input["command"].as_str().unwrap_or("");
                        if let Some(p) = printed_path(cmd) {
                            if let Some(w) = shown.get(&p) {
                                backs += 1;
                                answered += *w as u32;
                            }
                        } else if cmd.contains(">") || cmd.contains("sed -i") || cmd.contains("mv ") {
                            // a command may have changed anything: what the model saw is no longer known
                            shown.clear();
                        }
                    }
                    "read" => {
                        let ps: Vec<String> = std::iter::once(input["path"].as_str())
                            .chain(input["paths"].as_array().into_iter().flatten().map(|p| p.as_str()))
                            .flatten()
                            .map(norm)
                            .collect();
                        for p in ps {
                            if let Some(w) = shown.get(&p) {
                                backs += 1;
                                answered += *w as u32;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        println!(
            "REPLAY {name}: edit/write replayed {replayed} (failed to apply {failed}); results holding the file whole {whole}; \
             read-backs {backs}, answered by a one-liner {answered}; data-file checks {data_lines} {csv_facts:?}"
        );
        // the data facts the hand commands printed, from the task's own csv
        if let Some(csv) = task.files.iter().find(|(p, _)| p.ends_with(".csv")) {
            let p = wd.path().join(&csv.0);
            let rows = csv.1.lines().count();
            println!("REPLAY {name}: task csv {} on disk now: {}", csv.0, nexo_tools::data_check::report(&p).trim());
            println!("REPLAY {name}: task csv {} original has {rows} lines (wc -l)", csv.0);
        }
    }
}

/// Scratch dir on disk under the Cargo target dir, never in the RAM-backed system temp dir.
fn test_tmpdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix("nexo-test-")
        .tempdir_in(nexo_core::compat::scratch_base(std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))))
}
