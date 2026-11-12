//! Reads that an edit has made out of date (engine::stale).

use super::*;

/// A session that reads `src/pricing.rs` and then `src/other.rs`, as big as each other, edits
/// `src/pricing.rs` (the edit fails with `failed`), and goes on with short steps.
fn edited_session(failed: bool) -> Vec<Value> {
    let mut msgs = vec![json!({"role": "user", "content": "Fix the rounding in the pricing code."})];
    msgs.extend(tool(1, "read_file", json!({"path": "src/pricing.rs"}), &source(0, 120), false));
    msgs.extend(tool(2, "read_file", json!({"path": "src/other.rs"}), &source(5000, 120), false));
    let edit = json!({"path": "src/pricing.rs", "old_str": "v1", "new_str": "w1"});
    msgs.extend(tool(3, "edit_file", edit, if failed { "no match" } else { "ok" }, failed));
    for k in 4..12 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    msgs
}

#[test]
fn a_read_an_edit_of_its_file_made_out_of_date_is_marked_and_the_view_stays_until_a_cut() {
    let msgs = edited_session(false);
    let mut on = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
    on.sync(&msgs);
    assert_eq!(on.superseded_by(result_in(&on, 2)), Some(("src/pricing.rs".to_string(), 5)));
    // another file's read is not out of date
    assert_eq!(on.superseded_by(result_in(&on, 4)), None);
    // with no cut the view is the one sent with the setting off, and the same on each request
    let mut off = Engine::new(params("stale=0;eoq=0;threshold=0").unwrap());
    off.sync(&msgs);
    let first = on.view();
    assert_eq!(first, off.view());
    assert_eq!(first, on.view());
    let mut more = msgs.clone();
    more.extend(step(12, "wc -l part12", "12 lines"));
    on.sync(&more);
    assert_eq!(&on.view()[..first.len()], &first[..]);
}

#[test]
fn past_the_cap_a_read_out_of_date_leaves_before_one_of_the_same_size_and_its_note_names_the_edit() {
    let msgs = edited_session(false);
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    let cap = total - 1;
    let floor = (total - est_str(&source(0, 120)) / 2) as f64 / cap as f64;
    for cut in ["age", "need"] {
        for recall in [false, true] {
            let given = format!("stale=1;threshold=0;nudge=0;brief=1;eoq=0;target={cap};floor={floor};cut={cut}");
            let mut e = Engine::new(params(&given).unwrap());
            if recall {
                e.tools(&[RECALL_TOOL.to_string()]);
            }
            e.sync(&msgs);
            let view = e.view();
            assert_eq!(output(&view, 4), source(5000, 120), "{cut}: the read of the other file stays");
            let back = if recall {
                "recall m00003 brings it back as it was"
            } else {
                "read the file again to see it as it is now"
            };
            assert_eq!(
                    output(&view, 2),
                    format!(
                        "[curator] read_file output left out here: the read of src/pricing.rs from before its edit in m00006; {back}"
                    ),
                    "{cut}"
                );
        }
    }
    // with the setting off, the cut by age takes the oldest messages, the other file's read with them
    let mut off =
        Engine::new(params(&format!("stale=0;threshold=0;nudge=0;eoq=0;target={cap};floor={floor}")).unwrap());
    off.sync(&msgs);
    assert!(!sent(&off.view()).contains(&source(5000, 120)));
}

#[test]
fn a_failed_edit_a_later_read_and_other_files_leave_a_read_as_it_was_and_a_part_read_goes_out_of_date() {
    let e = |msgs: &[Value]| {
        let mut e = Engine::new(params("stale=1;eoq=0").unwrap());
        e.sync(msgs);
        e
    };
    // a failed edit did not change the file
    let failed = e(&edited_session(true));
    assert_eq!(failed.superseded_by(result_in(&failed, 2)), None);
    // a read after the edit holds the file as it is
    let mut msgs = edited_session(false);
    msgs.extend(tool(20, "read_file", json!({"path": "./src/pricing.rs"}), &source(9000, 120), false));
    let after = e(&msgs);
    let last = msgs.len() - 1;
    assert_eq!(after.superseded_by(result_in(&after, last)), None);
    assert!(after.superseded_by(result_in(&after, 2)).is_some());
    // a part of the file, and a write by a shell command, or one under a longer path
    let mut msgs = vec![json!({"role": "user", "content": "Fix it."})];
    msgs.extend(tool(
        1,
        "read_file",
        json!({"path": "src/pricing.rs", "offset": 10, "limit": 20}),
        &source(0, 20),
        false,
    ));
    msgs.extend(tool(2, "read_file", json!({"path": "lib/pricing.rs"}), &source(100, 20), false));
    msgs.extend(step(3, "sed -i 's/v1/w1/' src/pricing.rs", "done"));
    assert!(same_file("/work/src/pricing.rs", "src/pricing.rs") && !same_file("src/pricing.rs", "lib/pricing.rs"));
    let part = e(&msgs);
    assert_eq!(part.superseded_by(result_in(&part, 2)), Some(("src/pricing.rs".to_string(), 5)));
    // a file of the same name elsewhere is another file
    assert_eq!(part.superseded_by(result_in(&part, 4)), None);
    // a chat without tools has nothing to mark
    let chat = e(&[json!({"role": "user", "content": "Hi"}), json!({"role": "assistant", "content": "Hello"})]);
    assert!(chat.edits.is_empty() && chat.stale_reads(&[0, 1]).is_empty());
}

#[test]
fn a_failed_partial_edit_leaves_the_written_file_and_its_pin_as_they_were() {
    let body = source(0, 30);
    for failed in [true, false] {
        let mut msgs = vec![json!({"role": "user", "content": "Set up config.json and read src/main.rs."})];
        msgs.extend(tool(1, "read_file", json!({"path": "src/main.rs"}), &source(400, 30), false));
        msgs.extend(tool(2, "write_file", json!({"path": "config.json", "content": body}), "ok", false));
        // an edit as big as the whole file, and a small one, of each file
        let big = json!({"path": "config.json", "old_str": body, "new_str": "x"});
        msgs.extend(tool(3, "edit_file", big, if failed { "no match" } else { "ok" }, failed));
        let small = json!({"path": "src/main.rs", "old_str": "v1", "new_str": "w1"});
        msgs.extend(tool(4, "edit_file", small, if failed { "no match" } else { "ok" }, failed));
        let mut e = Engine::new(params("pin=4000").unwrap());
        e.sync(&msgs);
        assert_eq!(e.wrote.contains_key("config.json"), failed, "failed={failed}");
        assert_eq!(e.written.contains_key("src/main.rs"), !failed, "failed={failed}");
        assert!(e.written.contains_key("config.json"), "failed={failed}");
    }
}

/// What a debugger showed of the program is out of date once a later call moves the program.
#[test]
fn what_a_debugger_looked_at_is_out_of_date_once_the_program_moves_on() {
    let mut msgs = vec![json!({"role": "user", "content": "Why does it crash?"})];
    msgs.extend(tool(1, "debug", json!({"command": "run ./target crash"}), "stopped at entry", false));
    msgs.extend(tool(2, "debug", json!({"command": "regs"}), "rax 0x1 rbx 0x2", false));
    msgs.extend(tool(3, "debug", json!({"command": "cont"}), "SIGSEGV in dbg_crash", false));
    msgs.extend(tool(4, "debug", json!({"command": "bt"}), "#0 dbg_crash", false));
    msgs.extend(tool(5, "debug", json!({"command": "note crash: null"}), "saved: noted 0x10: crash: null", false));
    let mut e = Engine::new(params("stale=1;eoq=0;threshold=0").unwrap());
    e.sync(&msgs);
    // `regs` (message 4) was before the `cont` in message 6; `bt` (message 8) has nothing after it that moves
    assert_eq!(e.superseded_by(result_in(&e, 4)), Some(("debug:".to_string(), 5)));
    assert_eq!(e.superseded_by(result_in(&e, 8)), None);
    // the stop itself and the run are not looks
    assert_eq!(e.superseded_by(result_in(&e, 6)), None);
    assert_eq!(e.superseded_by(result_in(&e, 2)), None);
}
