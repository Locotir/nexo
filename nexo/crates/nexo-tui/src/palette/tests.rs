use super::*;

#[test]
fn add_dir_offers_the_folders_of_what_is_typed() {
    let d = tempfile::tempdir().unwrap();
    for p in ["proj", "other/lib", "other/.git", ".hidden"] {
        std::fs::create_dir_all(d.path().join(p)).unwrap();
    }
    std::fs::write(d.path().join("notes.txt"), "").unwrap();
    let cwd = d.path().join("proj");
    let v = |t: &str| dir_values(&cwd, t).into_iter().map(|(v, _)| v).collect::<Vec<_>>();
    assert_eq!(v("../"), ["../other/", "../proj/", "../../"], "folders only, hidden left out");
    assert_eq!(v("../other/"), ["../other/lib/"]);
    assert!(v("../other/.").contains(&"../other/.git/".to_string()), "a typed dot shows hidden folders");
    assert_eq!(v(""), ["../"], "nothing in proj but the folder above");
    assert!(v("missing/").is_empty());
    assert_eq!(wanted("/add-dir ../ot", 14), Some((PaletteKind::Args("add-dir".into()), 9, "../ot".into())));
    let p = Palette::args("add-dir", "../ot", 9, dir_values(&cwd, "../ot"));
    assert_eq!(p.current(), Some("../other/"));
}

#[test]
fn ranks_commands_and_moves_the_selection() {
    let mut p = Palette::commands("mo", 1);
    assert_eq!(p.current(), Some("model"));
    p.down();
    assert_ne!(p.current(), Some("model"));
    p.up();
    assert_eq!(p.current(), Some("model"));
    let (rows, sel) = p.rows(3);
    assert_eq!(sel, 0);
    assert!(rows[0].hint.contains("set one up"));
    assert_eq!(rows[0].args, "[name]");
    assert_eq!(rows[0].alias, None);
    assert_eq!(rows[0].hits, vec![0, 1]);
}

#[test]
fn an_alias_finds_its_command_first_and_tab_completes_the_primary_name() {
    let p = Palette::commands("ses", 1);
    assert_eq!(p.current(), Some("resume"), "Tab completes to the primary name");
    let (rows, _) = p.rows(3);
    assert_eq!(rows[0].label, "resume");
    assert_eq!(rows[0].alias.as_deref(), Some("sessions"));
    let p = Palette::commands("re", 1);
    let (rows, _) = p.rows(3);
    let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert!(labels.starts_with(&["resume", "rewind"]), "{labels:?}");
    assert!(rows.iter().take(2).all(|r| r.alias.is_none()));
}

#[test]
fn a_recent_command_is_listed_once_not_again_under_its_group() {
    let mut p = Palette::commands_full("", 1, Vec::new(), vec!["resume".into(), "rewind".into()]);
    let v = p.view(200);
    let labels: Vec<String> = v
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Item(r, _) => Some(r.label.clone()),
            _ => None,
        })
        .collect();
    for name in ["resume", "rewind"] {
        assert_eq!(labels.iter().filter(|l| *l == name).count(), 1, "{name} once: {labels:?}");
    }
    assert_eq!(p.len(), labels.len(), "no row is dropped from the list");
}

#[test]
fn with_nothing_typed_it_lists_recent_commands_then_every_group() {
    let mut p = Palette::commands_full("", 1, vec![("deploy".into(), "ship it".into())], vec!["theme".into()]);
    assert_eq!(p.current(), Some("theme"), "the most recent first");
    let v = p.view(4);
    assert_eq!(v.entries[0], Entry::Header("recent".into()));
    assert!(matches!(&v.entries[1], Entry::Item(r, 0) if r.label == "theme"));
    assert_eq!(v.entries[2], Entry::Header("common".into()));
    assert_eq!(v.above, 0);
    assert_eq!(v.below, p.len() - 2);
    // the titles scroll with their rows; the last section is one's own
    p.up();
    assert_eq!(p.current(), Some("deploy"));
    let v = p.view(4);
    assert_eq!(v.entries[v.entries.len() - 2], Entry::Header("your commands".into()));
    assert_eq!(v.below, 0);
    // typing drops the sections
    p.set_query("th");
    assert!(p.view(4).entries.iter().all(|e| matches!(e, Entry::Item(..))));
}

#[test]
fn a_short_screen_lists_commands_with_no_section_titles_unless_they_all_fit() {
    let mut p = Palette::commands_full("", 1, vec![], vec!["theme".into()]);
    assert!(p.view_titled(13, false).entries.iter().all(|e| matches!(e, Entry::Item(..))));
    assert_eq!(p.view_titled(13, false).entries.len(), 13);
    assert!(p.view_titled(13, true).entries.iter().any(|e| matches!(e, Entry::Header(_))));
    assert!(p.view_titled(500, false).entries.iter().any(|e| matches!(e, Entry::Header(_))), "all of it fits");
}

#[test]
fn the_list_scrolls_only_when_the_selection_leaves_it() {
    let mut p = Palette::files("", 1, (0..20).map(|i| format!("f{i:02}")).collect());
    assert_eq!(p.view(5).below, 15);
    for _ in 0..4 {
        p.down();
    }
    assert_eq!(p.view(5).above, 0, "still in sight");
    p.down();
    let v = p.view(5);
    assert_eq!((v.above, v.below), (1, 14));
    p.up();
    p.up();
    assert_eq!(p.view(5).above, 1, "going back up inside the window does not move it");
    p.page(true);
    assert_eq!(p.current(), Some("f16"));
    p.page(false);
    p.page(false);
    assert_eq!(p.current(), Some("f00"));
}

#[test]
fn a_typo_gets_a_suggestion() {
    let p = Palette::commands("modle", 1);
    assert!(p.is_empty());
    assert_eq!(p.suggestion(), Some("model"));
    assert_eq!(Palette::commands("mo", 1).suggestion(), None);
}

#[test]
fn lists_the_values_of_an_argument() {
    let vals = arg_values("permissions", &[], "", &[]);
    let p = Palette::args("permissions", "yo", 13, vals);
    assert_eq!(p.current(), Some("yolo"));
    assert!(p.runs_selection());
    assert_eq!(p.rows(3).0[0].hint, "never ask");
    let p = Palette::args("permissions", "ro", 13, arg_values("permissions", &[], "", &[]));
    assert_eq!(p.current(), Some("read-only"), "the letters in order");
    let models = vec![("ds".to_string(), "chat".to_string()), ("ol".to_string(), "qwen".to_string())];
    let p = Palette::args("model", "q", 7, arg_values("model", &models, "ds/chat", &[]));
    assert_eq!(p.current(), Some("ol/qwen"));
    assert!(!p.runs_selection(), "a model can be one not listed yet");
    let e = arg_values("effort", &[], "", &["low".into(), "max".into()]);
    assert_eq!(e.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["default", "low", "max"]);
    assert!(arg_values("theme", &[], "", &[]).iter().any(|(v, _)| v == "light"));
}

#[test]
fn lists_running_jobs_and_sessions_deleted_in_this_run() {
    let job = |id, kind, status| JobInfo {
        id,
        kind,
        label: format!("cargo build {id}\nmore"),
        status,
        elapsed_ms: 0,
        lines: 0,
    };
    let jobs = vec![
        job(3, JobKind::Shell, JobStatus::Running),
        job(4, JobKind::Shell, JobStatus::Killed),
        job(7, JobKind::Agent, JobStatus::Running),
    ];
    let v = live_values("jobs", &jobs, &[], &[], &[]);
    assert_eq!(v.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["kill 3", "kill 7"], "only running ones");
    assert_eq!(v[0].1, "stop: cargo build 3", "the first line of the command");
    assert_eq!(v[1].1, "stop sub-agent: cargo build 7");
    let t = vec!["s1".to_string(), "s2".to_string()];
    let u = live_values("undelete", &[], &t, &[], &[]);
    assert_eq!(u.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["s2", "s1"], "the last deleted first");
    assert!(u[0].1.contains("brings back"));
    assert!(live_values("theme", &jobs, &t, &[], &[]).is_empty());
    // /resume finds a session by a word of its title
    let s = vec![("20261101-1".to_string(), "parser fix".to_string()), ("20261102-1".to_string(), "docs".to_string())];
    let p = Palette::args("resume", "pars", 8, live_values("resume", &[], &[], &s, &[]));
    assert_eq!(p.current(), Some("20261101-1"));
    assert!(p.runs_selection(), "Enter opens it");
}

#[test]
fn remembers_recent_commands_once_each() {
    let h: Vec<String> =
        ["/theme light", "fix the parser", "/m x", "/sessions", "/theme dark", "/nope"].map(String::from).to_vec();
    assert_eq!(recent_commands(&h), ["theme", "resume", "model"]);
}

#[test]
fn knows_what_the_draft_wants() {
    assert_eq!(wanted("/mo", 3), Some((PaletteKind::Commands, 1, "mo".into())));
    assert_eq!(wanted("/model x", 8), Some((PaletteKind::Args("model".into()), 7, "x".into())));
    assert_eq!(wanted("/m  ds/c", 8), Some((PaletteKind::Args("model".into()), 4, "ds/c".into())));
    assert_eq!(wanted("/goal fix the tests", 19), None, "free text past the first word");
    assert_eq!(wanted("/compact x", 10), None, "no values to list");
    assert_eq!(wanted("/model x", 3), None, "the cursor is in the name");
    assert_eq!(wanted("look at @src/ma", 15), Some((PaletteKind::Files, 9, "src/ma".into())));
    assert_eq!(wanted("plain text", 10), None);
}

#[test]
fn the_file_picker_opens_only_at_the_start_of_a_word() {
    assert_eq!(wanted("ask me at bob@m", 15), None, "an email stays plain text");
    assert_eq!(wanted("git@github.com", 14), None, "user@host stays plain text");
    assert_eq!(wanted("@src", 4), Some((PaletteKind::Files, 1, "src".into())));
    assert_eq!(wanted("see (@src", 9), Some((PaletteKind::Files, 6, "src".into())));
    assert_eq!(wanted("see \"@src", 9), Some((PaletteKind::Files, 6, "src".into())));
    assert_eq!(wanted("a\n@x", 4), Some((PaletteKind::Files, 3, "x".into())));
}

#[test]
fn walks_a_project() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/main.rs"), "fn main(){}").unwrap();
    std::fs::write(d.path().join("README.md"), "x").unwrap();
    let f = project_files(d.path(), 100);
    assert_eq!(f, vec!["README.md", "src/main.rs"]);
}

#[test]
fn previews_the_selected_file_safely() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.rs"), "fn a() {}\n\x1b[31mred\x1b[0m\tx\nthird\n").unwrap();
    std::fs::write(d.path().join("b.bin"), [0u8, 1, 2, 3]).unwrap();
    let mut p = Palette::files("", 1, vec!["a.rs".into(), "b.bin".into()]);
    let pv = p.preview(d.path(), 2).cloned().unwrap();
    assert_eq!(pv.lines.len(), 2);
    assert_eq!(pv.lines[0], "fn a() {}");
    assert!(!pv.lines[1].contains('\x1b'), "{:?}", pv.lines[1]);
    assert!(pv.note.starts_with("3 lines"), "{}", pv.note);
    p.down();
    let pv = p.preview(d.path(), 2).cloned().unwrap();
    assert!(pv.lines.is_empty() && pv.note.starts_with("binary"), "{pv:?}");
    assert!(Palette::commands("", 1).preview(d.path(), 2).is_none());
}
