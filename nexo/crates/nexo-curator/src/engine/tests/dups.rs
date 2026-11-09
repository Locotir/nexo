//! Pointers to runs of lines an earlier output holds (engine::dups).

use super::*;

#[test]
fn the_output_that_sends_the_lines_of_a_copy_left_out_stays_in_that_pass_and_the_next() {
    let log =
        (0..200).map(|k| format!("event {} at {}", word(k + 2000), word(k + 3000))).collect::<Vec<_>>().join("\n");
    let mut msgs = vec![json!({"role": "user", "content": "What does the log hold?"})];
    msgs.extend(step(1, "cat run.log", &log));
    msgs.extend(step(2, "cat run.log", &log));
    for k in 3..8 {
        msgs.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
    }
    // how many of the log's distinct lines the view sends, and whether it says a later output sends a copy's lines
    let seen = |view: &[Value]| -> (usize, bool) {
        let text: String = view.iter().map(|m| crate::msg::flat(m.get("content")) + "\n").collect();
        let lines: FxHashSet<&str> = text.lines().filter(|l| l.starts_with("event ")).collect();
        (lines.len(), text.contains("a later output below sends"))
    };
    let mut whole = Engine::new(params("eoq=0;threshold=0;nudge=0").unwrap());
    whole.sync(&msgs);
    let total = whole.tokens();
    // a cap one token short, a floor that leaving one copy of the log is enough for, and every other part
    // a guess would let go (`threshold=1`): only the stub of the copy says the log is still sent
    let cap = total - 1;
    let floor = (total - est_str(&log) / 2) as f64 / cap as f64;
    for cut in ["age", "need", "copies", "cover"] {
        let given = format!("threshold=1;still=0;protect=2;nudge=0;sup=1;eoq=0;target={cap};floor={floor};cut={cut}");
        let mut e = Engine::new(params(&given).unwrap());
        e.sync(&msgs);
        let (lines, stub) = seen(&e.view());
        assert!(stub && !e.copied.is_empty(), "{cut}: {:?}", e.stats);
        assert_eq!(lines, 200, "{cut}: {:?}", e.stats);
        // the next requests weigh what to leave again: the output that sends the copy's lines stays
        let mut more = msgs.clone();
        for k in 9..12 {
            more.extend(step(k, &format!("wc -l part{k}"), &format!("{k} lines")));
            e.sync(&more);
            let (lines, stub) = seen(&e.view());
            assert!(!stub || lines == 200, "{cut}, request {k}: {lines} lines, {:?}", e.stats);
        }
    }
}

#[test]
fn an_output_sent_as_a_pointer_to_an_earlier_one_is_not_cut_for_lines_it_no_longer_sends() {
    // reads of the same lines again go as pointers to the first: cutting them frees next to nothing
    let mut e = reads("dedup=3;nudge=0;eoq=0;protect=0;threshold=1;still=0", &[(1, 40), (1, 40), (1, 40), (41, 80)]);
    e.view();
    let (t, k) = (e.tr.msgs.len(), e.tr.steps.len());
    let sizes = e.sizes_now();
    e.evict(t, k, &sizes, true);
    let view = e.render();
    for m in [4, 6] {
        assert!(output(&view, m).contains("the same as lines 1-40 of the output of"), "{m}: {view:?}");
    }
    assert!(output(&view, 8).contains("left out here"), "{view:?}");
}

#[test]
fn a_run_an_earlier_output_holds_points_to_it_while_that_output_is_whole() {
    let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60)]);
    let out = output(&e.view(), 4);
    let head = "[curator] 20 lines left out here: the same as lines 21-40 of the output of `sed -n '1,40p' listing.txt` above, from \"1108:";
    assert!(out.starts_with(head), "{out}");
    assert!(!out.contains("slot 30") && out.contains("slot 41\n") && out.ends_with("slot 60"), "{out}");
    assert!(reads("nudge=0", &[(1, 40), (21, 60)]).tokens() > e.tokens() + 100);
    // the earlier output is cut: the run is sent whole again
    let src = e.fz.by_msg(2)[0];
    e.dropped.entry(src).or_default().insert(0);
    assert_eq!(output(&e.render(), 4), listing(21, 60));
}

#[test]
fn an_output_asked_for_past_its_pointer_goes_whole() {
    let reads3 = |spec: &str| {
        let view = reads(spec, &[(1, 40), (1, 40), (1, 40)]).view();
        (output(&view, 4), output(&view, 6))
    };
    let (second, third) = reads3("dedup=3;nudge=0;asked=2");
    // the first copy asked again points up; asked past that pointer, the lines come whole
    assert!(second.starts_with("[curator] 40 lines left out here: the same as lines 1-40"), "{second}");
    assert_eq!(third, listing(1, 40));
    let (second, third) = reads3("dedup=3;nudge=0;asked=1");
    assert_eq!((second.as_str(), third.as_str()), (listing(1, 40).as_str(), listing(1, 40).as_str()));
    let (second, third) = reads3("dedup=3;nudge=0");
    assert!(second.starts_with("[curator]") && third.starts_with("[curator]"), "{second}\n{third}");
}

#[test]
fn the_output_just_arrived_goes_whole_and_points_up_from_the_next_request_on() {
    let mut e = reads("dedup=3;nudge=0;asked=3;fresh=1", &[(1, 40), (1, 40)]);
    // what the agent has just asked for comes as it is, with no pointer to send it looking
    assert_eq!(output(&e.view(), 4), listing(1, 40));
    let mut msgs = e.tr.msgs.clone();
    msgs.extend(step(9, "wc -l listing.txt", "40 listing.txt"));
    e.sync(&msgs);
    let view = e.view();
    assert!(output(&view, 4).starts_with("[curator] the same 40 lines as "), "{}", output(&view, 4));
    assert_eq!(output(&view, 6), "40 listing.txt");
}

#[test]
fn an_output_the_same_call_gave_before_goes_as_one_line_that_says_nothing_changed() {
    let mut e = reads("dedup=3;nudge=0;asked=3", &[(1, 40), (1, 40), (21, 60)]);
    let view = e.view();
    let again = output(&view, 4);
    assert!(again.starts_with("[curator] the same "), "{again}");
    assert!(again.contains("`sed -n '1,40p' listing.txt` gave above: nothing changed"), "{again}");
    assert!(!again.contains("slot 30"), "{again}");
    // another call whose output is mostly lines above was asked for: it goes whole
    assert_eq!(output(&view, 6), listing(21, 60));
    // once the copy above loses a part, the lines come back whole
    let src = e.fz.by_msg(2)[0];
    e.dropped.entry(src).or_default().insert(0);
    assert_eq!(output(&e.render(), 4), listing(1, 40));
}

#[test]
fn an_output_another_call_gave_before_goes_as_one_line_while_that_copy_is_whole() {
    let mut e = Engine::new(params("fresh=0;dedup=3;nudge=0;asked=3").unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 30 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "head -n 40 listing.txt", &listing(1, 40)));
    e.sync(&msgs);
    let again = output(&e.view(), 4);
    assert_eq!(again, "[curator] the same 40 lines as `sed -n '1,40p' listing.txt` gave above", "{again}");
    // once the copy above loses a part, the lines come back whole
    let src = e.fz.by_msg(2)[0];
    e.dropped.entry(src).or_default().insert(0);
    assert_eq!(output(&e.render(), 4), listing(1, 40));
}

#[test]
fn lines_read_many_times_go_once_after_their_first_output_leaves() {
    let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60), (21, 60), (21, 60)]);
    let src = e.fz.by_msg(2)[0];
    e.dropped.entry(src).or_default().insert(0);
    let view = e.render();
    // the first copy carries the lines again, the later ones point to it
    assert_eq!(output(&view, 4), listing(21, 60));
    for m in [6, 8] {
        let out = output(&view, m);
        assert!(out.starts_with("[curator] 20 lines left out here: the same as lines 1-20 of the output of `sed -n '21,60p' listing.txt` above"), "{out}");
        assert!(!out.contains("slot 30"), "{out}");
    }
}

#[test]
fn a_pointer_that_halves_its_lines_goes_in_their_place() {
    let mut e = reads("dedup=3;nudge=0", &[(1, 40), (11, 24)]);
    let out = output(&e.view(), 4);
    let (run, cost) = (est_str(&listing(11, 24)), est_str(&out));
    assert!(2 * cost <= run && run < 3 * cost, "{run} {cost}");
    assert!(out.starts_with("[curator] 14 lines left out here: the same as lines 11-24"), "{out}");
}

#[test]
fn a_read_made_after_its_first_output_lost_a_part_points_to_the_copy_that_sends_it() {
    let mut e = Engine::new(params("dedup=3;nudge=0;fresh=0").unwrap());
    let mut msgs = vec![json!({"role": "user", "content": "Read listing.txt and say what slot 50 holds."})];
    msgs.extend(step(1, "sed -n '1,40p' listing.txt", &listing(1, 40)));
    msgs.extend(step(2, "sed -n '21,60p' listing.txt", &listing(21, 60)));
    e.sync(&msgs);
    let src = e.fz.by_msg(2)[0];
    e.dropped.entry(src).or_default().insert(0);
    msgs.extend(step(3, "sed -n '21,60p' listing.txt", &listing(21, 60)));
    e.sync(&msgs);
    let view = e.render();
    assert_eq!(output(&view, 4), listing(21, 60));
    let out = output(&view, 6);
    assert!(out.contains("the same as lines 1-20 of the output of `sed -n '21,60p' listing.txt` above"), "{out}");
    assert!(!out.contains("slot 30"), "{out}");
}

#[test]
fn a_part_read_again_later_leaves_the_earlier_copy_and_the_latest_stays_whole() {
    let spec = "nudge=0;eoq=0;protect=0;threshold=0;margin=0;sup=1";
    let ranges = [(1, 40), (1, 40), (61, 100), (101, 140), (141, 180)];
    let mut e = reads(spec, &ranges);
    let view = e.view();
    assert!(output(&view, 2).contains("a later output below sends every line of it"), "{:?}", e.stats);
    assert_eq!(output(&view, 4), listing(1, 40));
    // without it, both copies stay
    let mut e = reads("nudge=0;eoq=0;protect=0;threshold=0;margin=0", &ranges);
    assert_eq!(output(&e.view(), 2), listing(1, 40));
}

#[test]
fn a_pointer_points_to_lines_sent_whole_and_short_runs_stay() {
    let mut e = reads("dedup=3;nudge=0", &[(1, 40), (21, 60), (22, 38), (42, 58), (58, 70)]);
    let view = e.view();
    // lines the second read sends as a pointer are found in the first
    assert!(output(&view, 6).contains("lines 22-38 of the output of `sed -n '1,40p' listing.txt`"), "{view:?}");
    assert!(output(&view, 8).contains("lines 22-38 of the output of `sed -n '21,60p' listing.txt`"), "{view:?}");
    // a line in common is fewer tokens than twice its pointer
    assert_eq!(output(&view, 10), listing(58, 70));
    assert_eq!(output(&reads("nudge=0", &[(1, 40), (21, 60)]).view(), 4), listing(21, 60), "off by default");
}
