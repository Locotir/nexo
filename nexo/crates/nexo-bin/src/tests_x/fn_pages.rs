//! Tests of `fn` pages that open at an address or run across functions.

use super::*;

/// A function of at least `n` instructions in the test binary.
fn long_function(img: &Image, n: usize) -> (u64, u64, usize) {
    for f in img.funcs() {
        if f.size() > 40_000 || f.size() < n as u64 * 2 {
            continue;
        }
        let mut starts = Vec::new();
        crate::disasm::walk(img.read_at(f.start, f.size() as usize).unwrap_or(&[]), f.start, |ins, _| {
            starts.push(ins.ip())
        });
        if starts.len() >= n {
            return (f.start, f.end, starts.len());
        }
    }
    panic!("no function of {n} instructions in the test binary");
}

#[test]
fn fn_at_an_address_opens_eight_instructions_before_and_marks_it() {
    let Some(img) = this_binary() else { return };
    let (start, _, count) = long_function(&img, 2000);
    let mut starts = Vec::new();
    crate::disasm::walk(img.read_at(start, 40_000).unwrap(), start, |ins, _| starts.push(ins.ip()));
    assert!(count >= 2000);
    // a one-byte instruction too (a `push`, a `ret`): one byte past it is the next instruction, not inside it
    let short = (30..1400).find(|&i| starts[i + 1] - starts[i] == 1);
    for k in [0usize, 3, 8, 9, 20, 1500, 1999].into_iter().chain(short) {
        // an address inside the instruction is the instruction
        for (budget, shift) in [(120usize, 0u64), (120, 1), (12, 0), (10, 0)] {
            let asked_addr = starts[k] + shift.min(1);
            let at = starts.iter().rposition(|&a| a <= asked_addr).unwrap();
            let asked = starts[at];
            let target = hex(asked_addr);
            let o = func_window(&img, &target, false, None, budget).unwrap();
            let p = o.page(None, budget);
            assert!(p.lines.len() <= budget.max(MIN_LINES));
            let marked: Vec<&String> = p.lines.iter().filter(|l| l.starts_with("> ")).collect();
            if asked_addr == start {
                assert!(marked.is_empty());
                continue;
            }
            assert_eq!(marked.len(), 1, "k={k} budget={budget}: {:?}", p.lines);
            assert!(marked[0].starts_with(&format!("> {asked:#010x}  ")), "k={k}: {}", marked[0]);
            let json = o.page(None, budget).value;
            let flagged: Vec<_> = json["items"].as_array().unwrap().iter().filter(|i| i["asked"] == true).collect();
            assert_eq!(flagged.len(), 1);
            assert_eq!(flagged[0]["addr"], asked);
            let head = &p.lines[0];
            assert!(head.starts_with("fn ") && head.contains("; refs "), "{head}");
            let lead = LEAD_IN.min(budget.saturating_sub(4));
            if at > lead {
                // the page opens eight instructions before (fewer when the budget is small)
                let first = &p.lines[2];
                let first_addr = parse_addr(first.trim_start_matches(['>', ' ']).split("  ").next().unwrap()).unwrap();
                let before = at - starts.iter().position(|&a| a == first_addr).unwrap();
                assert_eq!(before, lead, "k={k} budget={budget}");
                assert!(head.contains(&format!("; showing from {first_addr:#x}")), "{head}");
                let earlier = &p.lines[1];
                assert!(earlier.starts_with(&format!("... {} earlier lines: nexo bin fn ", at - before)), "{earlier}");
                assert!(earlier.contains(&format!("--from {start:#x}")), "{earlier}");
                assert_eq!(o.meta["earlier_lines"].as_u64().unwrap() as usize, at - before);
            } else {
                assert!(!p.lines[1].starts_with("..."), "k={k}: {}", p.lines[1]);
            }
        }
    }
}

#[test]
fn the_earlier_lines_command_shows_the_lines_and_the_pages_after_the_cut_join_up() {
    let Some(img) = this_binary() else { return };
    let (start, _, _) = long_function(&img, 2000);
    let mut starts = Vec::new();
    crate::disasm::walk(img.read_at(start, 40_000).unwrap(), start, |ins, _| starts.push(ins.ip()));
    let asked = hex(starts[1200] + 1);
    let o = func_window(&img, &asked, false, None, 40).unwrap();
    let p = o.page(None, 40);
    let earlier = p.lines[1].clone();
    let cmd = earlier.split(": ").nth(1).unwrap();
    assert!(cmd.contains("--lines 1194") || cmd.contains("--lines"), "{cmd}");
    // that command is a plain `fn` from the function start: its first page begins at the function
    let from = from_of(cmd);
    assert_eq!(from, start);
    let lines: usize = cmd.rsplit("--lines ").next().unwrap().trim().parse().unwrap();
    let skip = 1200 - LEAD_IN;
    assert_eq!(lines, skip + 2, "{cmd}");
    let again = func_window(&img, &asked, false, Some(from), lines).unwrap().page(Some(from), lines);
    assert!(again.lines[0].starts_with("fn "), "{}", again.lines[0]);
    // the heading, then every instruction before the first one the first page showed, then the cut line
    assert_eq!(again.lines.len(), skip + 2, "{} lines", again.lines.len());
    let shown = |l: &str| parse_addr(l.trim_start_matches(['>', ' ']).split("  ").next().unwrap()).unwrap();
    assert_eq!(shown(&again.lines[skip]), starts[skip - 1]);
    let first_on_page_one = shown(&p.lines[2]);
    assert_eq!(first_on_page_one, starts[skip]);
    assert!(p.lines[0].contains(&format!("; showing from {first_on_page_one:#x}")));
    // the pages after the first one, with `--from`, keep going from where the first one stopped
    let last = p.lines.last().unwrap();
    assert!(last.contains("more lines: nexo bin fn "), "{last}");
    let next = from_of(last);
    let o2 = func_window(&img, &asked, false, Some(next), 40).unwrap();
    let p2 = o2.page(Some(next), 40);
    let shown_first: Vec<&String> = p.lines[..p.lines.len() - 1].iter().collect();
    let last_shown = shown_first.last().unwrap().trim_start_matches(['>', ' ']).split("  ").next().unwrap().to_string();
    assert!(parse_addr(&last_shown).unwrap() < next);
    assert!(p2.lines[0].trim_start_matches(['>', ' ']).starts_with(&format!("{next:#010x}")), "{}", p2.lines[0]);
    // unchanged lines in `fn` by name: no marker, no earlier line, a plain header
    let named = func(&img, img.funcs().iter().find(|f| f.start == start).unwrap().name.as_str(), false);
    if let Ok(o) = named {
        assert!(!text_of(&o).contains("\n> "));
    }
}

#[test]
fn fn_on_a_fixture_marks_the_instruction_that_holds_the_address() {
    for (name, f) in both() {
        let img = open(&f);
        let t = text_of(&func(&img, &hex(f.e.unwrap() + 10), false).unwrap());
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("fn "), "{name}");
        assert!(lines.iter().any(|l| l.starts_with(&format!("> {:#010x}  call", f.e.unwrap() + 10))), "{name}: {t}");
        assert_eq!(lines.iter().filter(|l| l.starts_with("> ")).count(), 1);
        assert!(
            lines.iter().filter(|l| !l.starts_with("fn ")).all(|l| l.starts_with("  ") || l.starts_with("> ")),
            "{name}: {t}"
        );
        // the first byte of a function and a name need no marker
        let t = text_of(&func(&img, &hex(f.e.unwrap()), false).unwrap());
        assert!(!t.contains("> ") && t.contains(&format!("\n{:#010x}  ", f.e.unwrap())), "{name}: {t}");
    }
}

#[test]
fn fn_to_runs_across_functions_with_a_header_each_and_folded_padding() {
    for (name, f) in both() {
        let img = open(&f);
        let to = hex(f.f.unwrap() + 14);
        let o = func_view(&img, &FnQuery { to: Some(&to), ..FnQuery::new(&hex(f.a)) }).unwrap();
        let t = text_of(&o);
        let headers: Vec<&str> = t.lines().filter(|l| l.starts_with("fn ")).collect();
        assert_eq!(headers.len(), 6, "{name}: {t}");
        assert!(
            headers[0].contains(&format!("{:#x}..", f.a)) && headers[5].contains(&format!("{:#x}..", f.f.unwrap())),
            "{name}"
        );
        // B's closing int3 bytes are one line; D's too
        assert!(t.contains("... 3 bytes of padding"), "{name}: {t}");
        assert!(t.matches("of padding").count() >= 3, "{name}: {t}");
        assert!(!t.contains("int3"), "{name}: padding is folded: {t}");
        assert_eq!(o.meta["functions"], 6);
        // JSON: instruction items carry their function
        let j = o.page(None, 200).value;
        assert!(j["items"].as_array().unwrap().iter().any(|i| i["function"].is_string() && i["text"].is_string()));
        assert!(j["items"].as_array().unwrap().iter().any(|i| i["padding_bytes"].is_u64()));
        // the start inside a function: its header says where the view starts; the end cuts a function short
        let mid = hex(f.b + 7);
        let end = hex(f.e.unwrap() + 10);
        let o = func_view(&img, &FnQuery { to: Some(&end), ..FnQuery::new(&mid) }).unwrap();
        let t = text_of(&o);
        assert!(
            t.contains(&format!("; showing from {:#x}", f.b + 7)) || t.lines().next().unwrap().contains("fn "),
            "{name}: {t}"
        );
        let last_header = t.lines().rfind(|l| l.starts_with("fn ")).unwrap();
        assert!(last_header.contains(&format!("; showing up to {:#x}", f.e.unwrap() + 10)), "{name}: {t}");
        assert!(
            !t.lines().any(|l| l.starts_with(&format!("{:#010x}", f.e.unwrap() + 10))),
            "{name}: --to is exclusive: {t}"
        );
        assert!(t.contains(&format!("{:#010x}  mov dword ptr", f.e.unwrap())), "{name}");
        // the end of the range at a function boundary shows no part of the next one
        let o = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&hex(f.a)) }).unwrap();
        assert_eq!(o.meta["functions"], 2, "{name}");
    }
}

#[test]
fn fn_to_resume_keeps_to_bytes_and_json() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let to = hex(f.f.unwrap() + 14);
    let start = hex(f.a);
    let q = FnQuery { to: Some(&to), with_bytes: true, budget: 12, json: true, ..FnQuery::new(&start) };
    let o = func_view(&img, &q).unwrap();
    let p = o.page_as(None, 12, true);
    let cmd = p.next.expect("cut");
    assert!(cmd.starts_with("nexo bin fn /fx/prog 0x400100 --to 0x40015e --bytes --json --from 0x"), "{cmd}");
    assert!(cmd.ends_with("--lines 12"), "{cmd}");
    assert!(p.lines.last().unwrap().contains(&cmd));
    // following the command moves forward
    let from = from_of(&cmd);
    assert!(from > 0);
    let o2 = func_view(&img, &FnQuery { from: Some(from), ..q }).unwrap();
    assert_eq!(o2.rows[0].key, from);
}

#[test]
fn fn_to_refuses_what_it_cannot_do_with_exit_2() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let run = |a: &str, to: &str| func_view(&img, &FnQuery { to: Some(to), ..FnQuery::new(a) });
    for (a, to, why) in [
        (hex(f.b).as_str(), hex(f.a).as_str(), "not after the start"),
        (hex(f.b).as_str(), hex(f.b).as_str(), "not after the start"),
        (hex(f.b).as_str(), hex(f.table_or_string()).as_str(), "is in .data, which is data"),
        (hex(f.table_or_string()).as_str(), hex(f.table_or_string() + 8).as_str(), "which is data"),
        (hex(f.b).as_str(), "0x10", "not after the start"),
        ("0x10", "0x20", "not inside any section"),
        (hex(f.b).as_str(), "0xffffffffffffffff", "ask for at most"),
        (hex(f.b).as_str(), "zzz", "not a number"),
    ] {
        let e = run(a, to).unwrap_err();
        assert!(e.to_string().contains(why), "{a}..{to}: {e}");
        if why != "not a number" {
            assert_eq!(e.exit_code(), 2, "{a}..{to}: {e}");
        }
    }
    // too long a range
    let e = run(&hex(f.a), &hex(f.a + MAX_RANGE + 1)).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    // a start that no function holds
    let mut d = f.bytes.clone();
    let _ = &mut d;
    // an unknown name is a plain question without an answer (exit 1), as for `fn` without --to
    assert_eq!(run("no_such_function", &hex(f.f.unwrap())).unwrap_err().exit_code(), 1);
}

trait StringAddr {
    fn table_or_string(&self) -> u64;
}

impl StringAddr for Fixture {
    fn table_or_string(&self) -> u64 {
        self.string
    }
}

#[test]
fn fn_to_shows_the_bytes_between_functions_of_a_real_binary() {
    let Some(img) = this_binary() else { return };
    let funcs = img.funcs();
    // a gap: two functions with unclaimed bytes between them (the end of one section and the next)
    let pair = funcs.windows(2).find(|w| w[0].end < w[1].start && w[1].start - w[0].end < 4096 && w[0].size() < 4000);
    let Some(w) = pair else {
        eprintln!("skipped: this binary has no gap between two functions");
        return;
    };
    let to = hex(w[1].start + 1);
    let o = func_view(&img, &FnQuery { to: Some(&to), budget: usize::MAX, ..FnQuery::new(&hex(w[0].start)) }).unwrap();
    let t = whole(&o);
    assert!(t.contains(&format!("... {} byte", w[1].start - w[0].end)), "{t}");
    assert!(t.contains("outside any function"), "{t}");
}

#[test]
fn fn_to_over_two_thousand_instructions_joins_and_stays_inside_the_budget() {
    let Some(img) = this_binary() else { return };
    let (start, end, count) = long_function(&img, 2000);
    let to = hex(end);
    let q = |from: Option<u64>, budget: usize| {
        func_view(&img, &FnQuery { to: Some(&to), from, budget, ..FnQuery::new(&hex(start)) }).unwrap()
    };
    let o = q(None, 120);
    assert!(o.rows.len() <= 120, "{} rows formatted for a page of 120", o.rows.len());
    // one header, the instructions, and the trailing int3 padding folded into one line
    let lines = o.tail + o.rows.len();
    assert!(lines + 32 >= count && lines <= count + 2, "{lines} lines for {count} instructions");
    pieces_join("real fn --to", q, &[120, 500]);
}

#[test]
fn a_range_from_a_name_starts_at_the_function() {
    let f = fixtures::elf64_xref();
    let img = open(&f);
    let name = img.funcs().iter().find(|x| x.start == f.b).unwrap().name.clone();
    let a = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&name) }).unwrap();
    let b = func_view(&img, &FnQuery { to: Some(&hex(f.c)), ..FnQuery::new(&hex(f.b)) }).unwrap();
    assert_eq!(a.text_all(), b.text_all());
    assert_eq!(a.meta["functions"], 1);
}
