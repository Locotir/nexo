use super::*;

fn fresh(task: &Task) -> tempfile::TempDir {
    let wd = tempfile::tempdir().unwrap();
    task.materialize(wd.path()).unwrap();
    wd
}

/// Tests use only an already cached book: no download from the test run.
fn book_cached() -> bool {
    cache_file().is_some_and(|p| cached_ok(&p))
}

fn by_id(id: &str) -> Task {
    tasks().into_iter().find(|t| t.id == id).unwrap()
}

fn failing(parts: Vec<Part>) -> Vec<String> {
    parts.into_iter().filter(|(ok, _)| !ok).map(|(_, why)| why).collect()
}

#[test]
fn roman_numerals_and_labels_parse() {
    assert_eq!(roman("XLV"), Some(45));
    assert_eq!(roman("LIII"), Some(53));
    assert_eq!(roman("primero"), Some(1));
    assert_eq!(roman("12"), Some(12));
    let c =
        cites("- Part II, Capítulo X: «como se pareció cuando dijo»\nII.3: \"la aventura de los molinos\"\nno quote");
    assert_eq!(c.len(), 2);
    assert_eq!((c[0].part, c[0].chapter), (Some(2), Some(10)));
    assert_eq!((c[1].part, c[1].chapter), (Some(2), Some(3)));
    assert_eq!(norm("Frestón, ¡el SABIO!"), "freston el sabio");
}

#[test]
fn the_book_splits_into_two_parts_of_52_and_74_chapters() {
    if !book_cached() {
        return;
    }
    let b = chapters(&book_raw().unwrap());
    assert_eq!(b.keys().filter(|k| k.0 == 1).count(), 52);
    assert_eq!(b.keys().filter(|k| k.0 == 2).count(), 74);
    assert!(norm(&b[&(1, 8)]).contains("molinos de viento"));
    // the proverb is in four chapters, only one is the end of the government
    let n = norm("desnudo nací, desnudo me hallo");
    let hits: Vec<_> = b.iter().filter(|(_, t)| norm(t).contains(&n)).map(|(k, _)| *k).collect();
    assert!(hits.contains(&(2, 53)) && hits.len() >= 3, "{hits:?}");
}

const BOOK_TASKS: [&str; 3] = ["quixote-windmills", "quixote-island", "quixote-insula-thread"];

/// What someone who knows the novel by heart, and has not read this copy, would write.
const MEMORY_WINDMILLS: &str = "VERDICT: yes\n\n\
Part I, chapter VIII: \"descubrieron treinta o cuarenta molinos de viento que hay en aquel campo\"\n\
Part I, chapter VIII: \"aquel sabio Frestón que me robó el aposento y los libros ha vuelto estos gigantes en molinos\"\n\
Part I, chapter I: \"del poco dormir y del mucho leer, se le secó el celebro, de manera que vino a perder el juicio\"\n\
Part II, chapter X: \"como se pareció cuando dijo que los molinos de viento eran gigantes\"\n";

#[test]
fn closed_variants_have_no_book_and_the_same_question() {
    for id in BOOK_TASKS {
        let open = by_id(id);
        let closed = by_id(&format!("{id}-closed"));
        assert_eq!(closed.group, "closed");
        assert!(closed.setup.is_none() && closed.files.is_empty());
        let wd = fresh(&closed);
        assert_eq!(std::fs::read_dir(wd.path()).unwrap().count(), 0, "{id}: closed folder is not empty");
        assert!(!wd.path().join("quijote.txt").exists());
        assert!(closed.turns[0].1.contains("NOT in this folder"));
        assert!(closed.turns[0].1.ends_with(&open.turns[0].1));
        let wd = fresh(&open);
        assert!(wd.path().join("quijote.txt").exists(), "{id}: the open task has its book");
    }
}

#[test]
fn closed_variants_grade_like_the_open_task_and_memory_does_not_pass() {
    if !book_cached() {
        return;
    }
    for id in BOOK_TASKS {
        let t = by_id(&format!("{id}-closed"));
        let wd = fresh(&t);
        assert!(!(t.check)(wd.path()).ok, "{id}-closed: an empty answer passes");
        (t.solution)(wd.path());
        let got = (t.check)(wd.path());
        assert!(got.ok && got.score == 1.0, "{id}-closed: {}", got.detail);
    }
    // the answer from memory of the real book fails where the edits are
    let t = by_id("quixote-windmills-closed");
    let wd = fresh(&t);
    write_answer(wd.path(), MEMORY_WINDMILLS);
    let f = failing(windmill_parts(wd.path()));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    assert!(!(t.check)(wd.path()).ok);
    // the same, with memory of the island chain
    let t = by_id("quixote-island-closed");
    let wd = fresh(&t);
    let memory = island_answer().replace(variant("kingdom"), "el reino de Dinamarca o el de Soliadisa");
    write_answer(wd.path(), &memory);
    assert!(!(t.check)(wd.path()).ok);
    let memory = island_answer().replace(variant("town"), "mil vecinos, que era");
    write_answer(wd.path(), &memory);
    assert!(!(t.check)(wd.path()).ok);
    // and for the thread: the real book's sentences, not the edited copy's
    let real = thread_answer(&chapters(&book_raw().unwrap()));
    write_answer(wd.path(), &real);
    let f = failing(thread_parts(wd.path()));
    assert!(!f.is_empty(), "the real book's thread passes");
}

#[test]
fn the_perturbation_is_deterministic_and_minimal() {
    if !book_cached() {
        return;
    }
    let raw = book_raw().unwrap();
    let a = perturb(&raw).unwrap();
    assert_eq!(a, perturb(&raw).unwrap(), "the perturbation is not repeatable");
    // the same edits drawn again give the same variants
    for e in &EDITS {
        assert_eq!(variant(e.id), variant(e.id));
        assert!(e.variants.contains(&variant(e.id)));
    }
    // minimal: same lines but one per edit, every other line byte-identical
    let (ra, rb): (Vec<&str>, Vec<&str>) = (raw.lines().collect(), a.lines().collect());
    assert_eq!(ra.len(), rb.len());
    let changed: Vec<usize> = (0..ra.len()).filter(|&i| ra[i] != rb[i]).collect();
    assert_eq!(changed.len(), EDITS.len(), "{changed:?}");
    let grown = a.len() as i64 - raw.len() as i64;
    assert!(0 < grown && grown < 1_500, "{grown}");
    // every edit shows in the book and the real wording is gone from the swaps
    for e in &EDITS {
        assert!(a.contains(variant(e.id)));
        if !e.insert {
            assert!(!a.contains(e.find), "{} is still in the book", e.find);
        }
    }
    // a book where an anchor is missing is refused
    assert!(perturb("nothing to find").is_err());
}

#[test]
fn the_thread_is_spread_over_the_whole_book() {
    if !book_cached() {
        return;
    }
    let book = chapters(&book_text().unwrap());
    let fifth = fifths(&book);
    let with: Vec<(u32, u32)> = book.iter().filter(|(_, t)| norm(t).contains("insula")).map(|(k, _)| *k).collect();
    let per_part = |p: u32| with.iter().filter(|k| k.0 == p).count();
    let mut per_fifth = [0usize; 5];
    for k in &with {
        per_fifth[fifth[k]] += 1;
    }
    let kb: usize = book.values().map(String::len).sum::<usize>() / 1000;
    eprintln!(
        "book {kb} kB; chapters with an ínsula: {} (Part I {}, Part II {}); per fifth {per_fifth:?}",
        with.len(),
        per_part(1),
        per_part(2)
    );
    assert!(per_fifth.iter().all(|&n| n > 0), "{per_fifth:?}");
    assert!(with.len() >= THREAD_MIN + 8, "enough chapters to choose from: {}", with.len());
    assert!(per_part(1) >= THREAD_PART_MIN.0 + 4 && per_part(2) >= THREAD_PART_MIN.1 + 4);
    // the edited sentences are in chapters that had no ínsula before
    let real = chapters(&book_raw().unwrap());
    for id in ["count", "boat", "wood"] {
        let at = book.iter().find(|(_, t)| norm(t).contains(&norm(variant(id)))).map(|(k, _)| *k).unwrap();
        assert!(!norm(&real[&at]).contains("insula"), "{id}: chapter {at:?} already had an ínsula");
    }
}

#[test]
fn book_tasks_build_pass_their_reference_and_fail_otherwise() {
    if !book_cached() {
        return;
    }
    for id in BOOK_TASKS {
        let t = by_id(id);
        let wd = fresh(&t);
        assert!(std::fs::metadata(wd.path().join("quijote.txt")).unwrap().len() > 1_500_000);
        assert!(!(t.check)(wd.path()).ok, "{id}: the untouched task passes");
        (t.solution)(wd.path());
        let got = (t.check)(wd.path());
        assert!(got.ok && got.score == 1.0, "{id}: {}", got.detail);
    }
}

#[test]
fn windmill_decoys_fail() {
    if !book_cached() {
        return;
    }
    let t = by_id("quixote-windmills");
    let wd = fresh(&t);
    let run = |text: &str| {
        write_answer(wd.path(), text);
        failing(windmill_parts(wd.path()))
    };
    let good = windmill_answer();
    // the book was edited: the count from memory is not in this copy
    let f = run(MEMORY_WINDMILLS);
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    // the other chapter VIII (Part Two) is not where the windmills are
    let f = run(&good.replace("Part I, chapter VIII: \"aquel", "Part II, chapter VIII: \"aquel"));
    assert!(f.iter().any(|w| w.contains("mislabelled")), "{f:?}");
    // an invented quote
    let f = run(&good.replace("que hay en aquel campo", "que había en aquel campo"));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    // right quote, right book, wrong chapter for the memory of Part Two
    let f = run(&good.replace("Part II, chapter X:", "Part II, chapter XI:"));
    assert!(!f.is_empty());
    // the wrong verdict
    let f = run(&good.replace("VERDICT: yes", "VERDICT: no"));
    assert_eq!(f.len(), 1, "{f:?}");
    // the windmill sentence without the count is not the evidence asked for
    let f = run(&good.replace(&format!("descubrieron {} que hay", variant("windmills")), "descubrieron unos que hay"));
    assert!(!f.is_empty());
    // an ellipsis is tolerated, accents and case are not held against a quote
    let f = run(&good.replace(
        "aquel sabio Frestón que me robó el aposento y los libros ha vuelto",
        "AQUEL SABIO FRESTON que me robo ... ha vuelto",
    ));
    assert!(f.is_empty(), "{f:?}");
}

#[test]
fn island_decoys_fail() {
    if !book_cached() {
        return;
    }
    let t = by_id("quixote-island");
    let wd = fresh(&t);
    let good = island_answer();
    let run = |text: &str| {
        write_answer(wd.path(), text);
        failing(island_parts(wd.path()))
    };
    // the same proverb, real but from the chapter where Sancho first says it (Part II chapter VIII)
    let f = run(&good.replace("Part II, chapter LIII", "Part II, chapter VIII"));
    assert!(f.len() == 1 && f[0].contains("how it ends"), "{f:?}");
    let f = run(&good.replace("Barataria\n", "Baratario\n"));
    assert!(!f.is_empty());
    let f = run(&good.replace("GIVER: the Duke", "GIVER: the Priest"));
    assert!(!f.is_empty());
    let f = run(&good.replace("Part I, chapter X:", "Part I, chapter XI:"));
    assert!(f.iter().any(|w| w.contains("mislabelled")), "{f:?}");
    // memory: the real kingdom and the real size of the town are not in this copy
    let f = run(&good.replace(variant("kingdom"), "el reino de Dinamarca o el de Soliadisa"));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    let f = run(&good.replace(variant("town"), "mil vecinos, que era"));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
}

#[test]
fn thread_decoys_fail() {
    if !book_cached() {
        return;
    }
    let t = by_id("quixote-insula-thread");
    let wd = fresh(&t);
    (t.solution)(wd.path());
    let good = read(wd.path(), "ANSWER.md");
    let lines: Vec<&str> = good.lines().collect();
    assert!(lines.len() >= THREAD_MIN + 8, "{}", lines.len());
    let run = |text: &str| {
        write_answer(wd.path(), text);
        failing(thread_parts(wd.path()))
    };
    assert!(run(&good).is_empty());
    // too few chapters: only the first half of the list
    let f = run(&lines[..lines.len() / 2].join("\n"));
    assert!(!f.is_empty());
    // all of Part I only: not spread over the book
    let part1: Vec<&str> = lines.iter().copied().filter(|l| l.starts_with("Part I,")).collect();
    let f = run(&part1.join("\n"));
    assert!(f.iter().any(|w| w.contains("every fifth") || w.contains("Part II")), "{f:?}");
    // one line mislabelled or invented spoils the whole answer
    let f = run(&good.replacen("Part I, chapter", "Part II, chapter", 1));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    let f = run(&format!("{good}Part II, chapter X: \"una ínsula que Cervantes jamás escribió en este capítulo\"\n"));
    assert!(f.iter().any(|w| w.contains("invented")), "{f:?}");
    // short quotes and quotes about something else do not count
    let f = run(&format!("{good}Part I, chapter VII: \"alguna ínsula, y le dejase a él por gobernador\"\n"));
    assert!(f.iter().any(|w| w.contains("shorter")), "{f:?}");
    let f = run(&format!("{good}Part I, chapter VIII: \"aquel sabio Frestón que me robó el aposento y los libros ha vuelto estos gigantes en molinos\"\n"));
    assert!(f.iter().any(|w| w.contains("not about an")), "{f:?}");
    // the same chapter ten times is one chapter
    let f = run(&vec![lines[0]; 40].join("\n"));
    assert!(f.iter().any(|w| w.contains("different chapters")), "{f:?}");
    // without the edited sentences the answer is incomplete
    let real = chapters(&book_raw().unwrap());
    let keep: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| {
            let c = &cites(l)[0];
            norm(&real[&(c.part.unwrap(), c.chapter.unwrap())]).contains(&norm(&c.quote))
        })
        .collect();
    assert!(keep.len() < lines.len());
    let f = run(&keep.join("\n"));
    assert!(f.iter().any(|w| w.contains("only this copy")), "{f:?}");
}

#[test]
fn the_app_is_modest_and_the_share_request_reference_passes() {
    let t = by_id("share-request");
    let lines: usize = t.files.iter().map(|(_, s)| s.lines().count()).sum();
    assert!((150..=300).contains(&lines), "{lines}");
    let wd = fresh(&t);
    assert!(!(t.check)(wd.path()).ok);
    (t.solution)(wd.path());
    let got = (t.check)(wd.path());
    assert!(got.ok && got.score == 1.0, "{}", got.detail);
}

#[test]
fn share_request_decoys_fail() {
    let t = by_id("share-request");
    let wd = fresh(&t);
    std::fs::create_dir_all(wd.path().join("docs")).unwrap();
    let run = |text: &str| {
        std::fs::write(wd.path().join("docs/share-request.http"), text).unwrap();
        failing(request_parts(wd.path()))
    };
    let lf = REQUEST.replace("\r\n", "\n");
    assert!(run(&lf).is_empty());
    assert!(run(&format!("```http\n{lf}```\n")).is_empty());
    // the deprecated v1 route: other path, PHPSESSID, _csrf, form body
    let v1 = "POST /api/v1/notes/42/share HTTP/1.1\nHost: notes.example.test\nCookie: PHPSESSID=x\nContent-Type: application/x-www-form-urlencoded\n\n_csrf=t&to=sam%40example.test&level=read\n";
    assert!(run(v1).len() >= 6);
    // one wrong fact each
    assert_eq!(run(&lf.replace("/api/v2/", "/api/v1/")).len(), 1);
    assert_eq!(run(&lf.replace("nbsid", "nb_session")).len(), 1);
    assert_eq!(run(&lf.replace("X-NB-CSRF", "X-CSRF-Token")).len(), 1);
    assert_eq!(run(&lf.replace("?notify=1", "")).len(), 1);
    assert_eq!(run(&lf.replace("X-NB-Client: web\n", "")).len(), 1);
    assert_eq!(run(&lf.replace("application/json", "application/x-www-form-urlencoded")).len(), 1);
    assert_eq!(run(&lf.replace("\"permission\"", "\"level\"")).len(), 1);
    assert_eq!(run(&lf.replace("viewer", "read")).len(), 1);
    assert_eq!(run(&lf.replace("\"expires_days\": 7", "\"expires_days\": 90")).len(), 1);
    assert_eq!(run(&lf.replace("POST", "PUT")).len(), 1);
}
