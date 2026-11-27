use nexo_core::ToolDisplay;

use super::show::*;
use super::*;
use crate::util::MAX_FILE_BYTES;

#[tokio::test]
async fn a_miss_only_in_whitespace_shows_the_files_own_text_and_where() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let file = "fn a() {\n\tlet x = 1;  \n\tx\n}\nfn b() {\n    let y = 2;\n}\n";
    std::fs::write(d.path().join("a.rs"), file).unwrap();
    // trailing spaces in the file
    let out = Edit.run(json!({"path": "a.rs", "old_string": "\tlet x = 1;\n\tx", "new_string": "z"}), &c).await;
    assert!(out.is_error);
    assert!(
        out.content.contains("lines 2-3 match it except for spaces or tabs at the ends of lines"),
        "{}",
        out.content
    );
    assert!(out.content.contains("The file has there:\n\tlet x = 1;  \n\tx\nCopy"), "{}", out.content);
    // tabs given where the file has spaces
    let out = Edit.run(json!({"path": "a.rs", "old_string": "\tlet y = 2;", "new_string": "z"}), &c).await;
    assert!(out.content.contains("as given; line 6 matches it"), "{}", out.content);
    assert!(out.content.contains("in the indentation"), "{}", out.content);
    // two places that match up to whitespace: no hint, so it never points at the wrong one
    std::fs::create_dir(d.path().join("sub")).unwrap();
    std::fs::write(d.path().join("sub/b.rs"), "  x\n\tx\n").unwrap();
    let out = Edit.run(json!({"path": "sub/b.rs", "old_string": "x ", "new_string": "z"}), &c).await;
    assert!(out.content.ends_with("read the file and copy the text exactly"), "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), file, "nothing written");
}

#[tokio::test]
async fn replaces_exactly_one_occurrence_and_reports_a_diff() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("a.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let out = Edit.run(json!({"path": "a.rs", "old_string": "fn b() {}", "new_string": "fn b() { 1 }"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.starts_with("edited a.rs: +1 −1 lines, applied and verified on disk\n"), "{}", out.content);
    assert!(out.content.contains("-fn b() {}\n+fn b() { 1 }"));
    assert_eq!(std::fs::read_to_string(d.path().join("a.rs")).unwrap(), "fn a() {}\nfn b() { 1 }\nfn c() {}\n");
    assert!(matches!(out.display, ToolDisplay::Diff { start_line: 1, .. }));
}

#[tokio::test]
async fn refuses_ambiguous_or_missing_text_unless_replace_all() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("a.txt"), "x\nx\n").unwrap();
    let out = Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y"}), &c).await;
    assert!(out.is_error && out.content.contains("occurs 2 times"), "{}", out.content);
    let out = Edit.run(json!({"path": "a.txt", "old_string": "z", "new_string": "y"}), &c).await;
    assert!(out.is_error && out.content.contains("not found"));
    let out = Edit.run(json!({"path": "a.txt", "old_string": "x", "new_string": "y", "replace_all": true}), &c).await;
    assert!(!out.is_error && out.content.contains("(2 occurrences)"), "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "y\ny\n");
    assert!(Edit
        .run(json!({"path": "nope", "old_string": "a", "new_string": "b"}), &c)
        .await
        .content
        .contains("does not exist"));
    assert!(Edit.run(json!({"path": "a.txt", "old_string": "a", "new_string": "a"}), &c).await.is_error);
    assert!(Edit.run(json!({"path": "a.txt", "old_string": "", "new_string": "a"}), &c).await.is_error);
    assert_eq!(Edit.summary(&json!({"path": "a.txt", "replace_all": true})), "edit a.txt (all occurrences)");
}

#[tokio::test]
async fn edits_a_crlf_file_and_keeps_its_line_ends() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
    let out = Edit.run(json!({"path": "w.txt", "old_string": "two\nthree", "new_string": "2\n3"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n3\r\n");
}

#[tokio::test]
async fn refuses_a_file_that_is_not_utf8_and_leaves_it_alone() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    // Latin-1: "café = 1\n"
    let latin1 = b"caf\xe9 = 1\nx = 2\n".to_vec();
    std::fs::write(d.path().join("l.txt"), &latin1).unwrap();
    let out = Edit.run(json!({"path": "l.txt", "old_string": "x = 2", "new_string": "x = 3"}), &c).await;
    assert!(out.is_error && out.content.contains("not UTF-8"), "{}", out.content);
    assert_eq!(std::fs::read(d.path().join("l.txt")).unwrap(), latin1);
}

#[tokio::test]
async fn a_mixed_line_end_file_keeps_every_line_end_it_had() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("m.txt"), "a\nb\r\nc\n").unwrap();
    let out = Edit.run(json!({"path": "m.txt", "old_string": "a", "new_string": "x"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "x\nb\r\nc\n");
    // a CRLF string against the plain part of the file
    let out = Edit.run(json!({"path": "m.txt", "old_string": "x\r\nb", "new_string": "y\r\nb"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("m.txt")).unwrap(), "y\nb\r\nc\n");
    // a file all CRLF: new lines in `new_string` take its ends
    std::fs::write(d.path().join("w.txt"), "one\r\ntwo\r\n").unwrap();
    let out = Edit.run(json!({"path": "w.txt", "old_string": "two", "new_string": "2\n2b"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("w.txt")).unwrap(), "one\r\n2\r\n2b\r\n");
}

#[tokio::test]
async fn a_file_over_the_limit_is_refused_before_it_is_read() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    // sparse and all NUL: read whole it would be called binary, not too large
    let f = std::fs::File::create(d.path().join("big.txt")).unwrap();
    f.set_len(MAX_FILE_BYTES + 1).unwrap();
    let out = Edit.run(json!({"path": "big.txt", "old_string": "a", "new_string": "b"}), &c).await;
    assert!(out.is_error && out.content.contains("too large"), "{}", out.content);
}

#[cfg(unix)]
#[tokio::test]
async fn an_edit_replaces_the_file_whole_keeping_its_mode_and_links() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let f = d.path().join("run.sh");
    std::fs::write(&f, "echo a\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o750)).unwrap();
    std::os::unix::fs::symlink(&f, d.path().join("link.sh")).unwrap();
    let out = Edit.run(json!({"path": "link.sh", "old_string": "a", "new_string": "b"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
    assert!(std::fs::symlink_metadata(d.path().join("link.sh")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o750);
    let names: Vec<String> =
        std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 2, "no temporary file left: {names:?}");
    // a read-only file stays read-only and unchanged
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
    let out = Edit.run(json!({"path": "run.sh", "old_string": "b", "new_string": "c"}), &c).await;
    assert!(out.is_error && out.content.contains("read-only"), "{}", out.content);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo b\n");
}

#[cfg(test)]
mod many_tests {
    use super::*;

    #[tokio::test]
    async fn several_changes_go_in_one_call_and_one_that_fails_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("a.rs");
        std::fs::write(&f, "fn a() {}\nfn b() {}\nlet x = 1; let x2 = x;\n").unwrap();
        let edits = json!([
            {"old_string": "fn a() {}", "new_string": "fn a() { 1 }"},
            // the second sees the text the first left
            {"old_string": "fn a() { 1 }\nfn b", "new_string": "fn a() { 1 }\nfn bee"},
            {"old_string": "x", "new_string": "y", "replace_all": true}
        ]);
        let out = Edit.run(json!({"path": "a.rs", "edits": edits}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited a.rs (3 changes): +3 −3 lines"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "fn a() { 1 }\nfn bee() {}\nlet y = 1; let y2 = y;\n");
        assert_eq!(Edit.summary(&json!({"path": "a.rs", "edits": edits})), "edit a.rs (3 changes)");

        let before = std::fs::read_to_string(&f).unwrap();
        let bad = json!([{"old_string": "fn bee", "new_string": "fn b"}, {"old_string": "missing", "new_string": "z"}]);
        let out = Edit.run(json!({"path": "a.rs", "edits": bad}), &c).await;
        assert!(out.is_error);
        assert!(out.content.starts_with("edit: change 2 of 2: `old_string` not found in a.rs"), "{}", out.content);
        assert!(out.content.ends_with("nothing was written"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), before);

        for (args, want) in [
            (
                json!({"path": "a.rs", "edits": [{"old_string": "", "new_string": "z"}]}),
                "edit: change 1: `old_string` is empty",
            ),
            (json!({"path": "a.rs", "edits": "x"}), "edit: `edits` must be a list"),
            (json!({"path": "a.rs", "edits": [], "check": "true"}), "edit: `edits` is empty, so nothing changes"),
            (
                json!({"path": "a.rs", "old_string": "a", "new_string": "b", "edits": [{"old_string": "a", "new_string": "b"}]}),
                "edit: give either",
            ),
            (json!({"path": "a.rs"}), "edit: `old_string` and `new_string` (strings) are required"),
        ] {
            let out = Edit.run(args, &c).await;
            assert!(out.is_error && out.content.starts_with(want), "{want}: {}", out.content);
        }
    }
}

#[cfg(test)]
mod around_tests {
    use super::*;

    #[tokio::test]
    async fn short_files_come_back_whole_after_an_edit() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let short: String = (1..=60).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("s.txt"), &short).unwrap();
        let out = Edit.run(json!({"path": "s.txt", "old_string": "line30\n", "new_string": "thirty\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.lines().next().unwrap().contains("applied and verified on disk"), "{}", out.content);
        for i in [1, 29, 30, 31, 60] {
            let want = if i == 30 { "thirty".to_string() } else { format!("line{i}") };
            assert!(out.content.contains(&format!("{i:>6}│ {want}\n")), "line {i}: {}", out.content);
        }
        assert!(!out.content.contains("…"), "{}", out.content);
        let long: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("l.txt"), &long).unwrap();
        let out = Edit.run(json!({"path": "l.txt", "old_string": "line100\n", "new_string": "hundred\n"}), &c).await;
        assert!(out.content.contains("   100│ hundred\n"), "{}", out.content);
        assert!(!out.content.contains("│ line1\n") && !out.content.contains("│ line200\n"), "{}", out.content);
    }

    #[tokio::test]
    async fn the_result_shows_the_numbered_lines_around_the_change() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let file: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(d.path().join("a.txt"), &file).unwrap();
        let out = Edit.run(json!({"path": "a.txt", "old_string": "line10\n", "new_string": "ten\nten b\n"}), &c).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("\nfile now:\n     7│ line7\n"), "{}", out.content);
        assert!(out.content.contains("    10│ ten\n    11│ ten b\n    12│ line11\n"), "{}", out.content);
        assert!(out.content.ends_with("    14│ line13\n"), "{}", out.content);
        assert!(!out.content.contains("line6\n    7"), "{}", out.content);
    }

    #[test]
    fn the_snippet_is_capped_and_gaps_are_marked() {
        let old: String = (1..=200).map(|i| format!("l{i}\n")).collect();
        // a change at the top and one far below: two regions with a gap
        let two = old.replace("l2\n", "X\n").replace("l150\n", "Y\n");
        let s = around(&old, &two);
        assert!(s.contains("…\n") && s.contains("   150│ Y\n") && s.contains("     2│ X\n"), "{s}");
        // every line changed: cut at the cap
        let all: String = (1..=200).map(|i| format!("n{i}\n")).collect();
        let s = around(&old, &all);
        assert_eq!(s.lines().filter(|l| l.contains('│')).count(), SNIPPET_LINES, "{s}");
        assert!(s.ends_with("…\n"), "{s}");
        assert_eq!(around("a\n", "a\n"), "");
    }

    #[tokio::test]
    async fn edit_files_changes_several_files_and_stops_at_the_first_failure() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        for (n, t) in [("a.txt", "one\n"), ("b.txt", "two\n"), ("c.txt", "three\n")] {
            std::fs::write(d.path().join(n), t).unwrap();
        }
        let out = Edit
            .run(
                json!({"files": [
                    {"path": "a.txt", "edits": [{"old_string": "one", "new_string": "1"}]},
                    {"path": "b.txt", "old_string": "two", "new_string": "2"}
                ]}),
                &c,
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.starts_with("edited 2 files: a.txt, b.txt"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("b.txt")).unwrap(), "2\n");
        // the second file fails: the first stays applied, the third is not tried, and the answer says so
        let out = Edit
            .run(
                json!({"files": [
                    {"path": "a.txt", "old_string": "1", "new_string": "uno"},
                    {"path": "b.txt", "old_string": "absent", "new_string": "x"},
                    {"path": "c.txt", "old_string": "three", "new_string": "3"}
                ]}),
                &c,
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("applied before the failure: a.txt; not tried: c.txt"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "uno\n");
        assert_eq!(std::fs::read_to_string(d.path().join("c.txt")).unwrap(), "three\n");
        // a malformed entry is found before anything is written
        let out = Edit.run(json!({"files": [{"path": "a.txt", "old_string": "uno", "new_string": "x"}, {"old_string": "a", "new_string": "b"}]}), &c).await;
        assert!(out.is_error && out.content.contains("files[2]"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "uno\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_check_runs_the_command_and_reports_exit_and_output() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::write(d.path().join("a.txt"), "one\n").unwrap();
        let out = Edit
            .run(json!({"path": "a.txt", "old_string": "one", "new_string": "two", "check": "cat a.txt; exit 3"}), &c)
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("\ncheck: cat a.txt; exit 3 | exit 3 | 0.") && out.content.ends_with("\ntwo"),
            "{}",
            out.content
        );
        // a failed change runs no check
        let out =
            Edit.run(json!({"path": "a.txt", "old_string": "zzz", "new_string": "y", "check": "echo ran"}), &c).await;
        assert!(out.is_error && !out.content.contains("check:"), "{}", out.content);
    }
}

#[cfg(test)]
mod elsewhere_tests {
    use super::*;

    /// The H1 logs: two changes sent for `src/report.rs`, one of them for text in `src/pricing.rs`.
    #[tokio::test]
    async fn a_text_that_lives_in_another_file_names_that_file() {
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/pricing.rs"), "use x;\n\npub fn margin(cost: f64) -> f64 {\n    cost\n}\n")
            .unwrap();
        std::fs::write(d.path().join("src/report.rs"), "pub fn format_row() {}\n").unwrap();
        let edits = json!([
            {"old_string": "pub fn format_row() {}", "new_string": "/// Row.\npub fn format_row() {}"},
            {"old_string": "pub fn margin(cost: f64) -> f64 {", "new_string": "/// Margin.\npub fn margin(cost: f64) -> f64 {"}
        ]);
        let out = Edit.run(json!({"path": "src/report.rs", "edits": edits}), &c).await;
        assert!(out.is_error && out.content.contains("change 2 of 2"), "{}", out.content);
        assert!(
            out.content.contains("it is in src/pricing.rs line 3") && out.content.contains("one file"),
            "{}",
            out.content
        );
        assert_eq!(std::fs::read_to_string(d.path().join("src/report.rs")).unwrap(), "pub fn format_row() {}\n");
        // in two files: no guess
        std::fs::write(d.path().join("src/other.rs"), "pub fn margin(cost: f64) -> f64 {\n").unwrap();
        let out = Edit.run(json!({"path": "src/report.rs", "edits": edits}), &c).await;
        assert!(out.is_error && !out.content.contains("it is in"), "{}", out.content);
    }

    #[tokio::test]
    async fn an_edit_that_changes_nothing_or_follows_an_outside_change_is_refused() {
        use crate::read::Read;
        let d = tempfile::tempdir().unwrap();
        let c = ToolContext::new(d.path().to_path_buf());
        let f = d.path().join("g.txt");
        std::fs::write(&f, "one\ntwo\n").unwrap();
        // same old and new: refused, file untouched
        let same = Edit.run(json!({"path": "g.txt", "old_string": "one", "new_string": "one"}), &c).await;
        assert!(same.is_error && same.content.contains("are the same"), "{}", same.content);
        // changes that cancel out leave the content as it was
        let edits = json!([{"old_string": "one", "new_string": "uno"}, {"old_string": "uno", "new_string": "one"}]);
        let none = Edit.run(json!({"path": "g.txt", "edits": edits}), &c).await;
        assert!(none.is_error && none.content.contains("would not change"), "{}", none.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\ntwo\n");
        // read, then two edits in a row by the model: fine
        assert!(!Read.run(json!({"path": "g.txt"}), &c).await.is_error);
        assert!(!Edit.run(json!({"path": "g.txt", "old_string": "one", "new_string": "1"}), &c).await.is_error);
        assert!(!Edit.run(json!({"path": "g.txt", "old_string": "two", "new_string": "2"}), &c).await.is_error);
        // changed behind its back: refused, nothing written; read again and it goes through
        std::fs::write(&f, "1\n2\nand a line the person added\n").unwrap();
        let stale = Edit.run(json!({"path": "g.txt", "old_string": "1", "new_string": "uno"}), &c).await;
        assert!(stale.is_error && stale.content.contains("`read` it again"), "{}", stale.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "1\n2\nand a line the person added\n");
        assert!(!Read.run(json!({"path": "g.txt"}), &c).await.is_error);
        assert!(!Edit.run(json!({"path": "g.txt", "old_string": "1", "new_string": "uno"}), &c).await.is_error);
    }
}

#[test]
fn an_old_string_that_overlaps_itself_is_not_taken_for_unique() {
    let err = apply::replace("aaa", "aa", "b", false, "f").unwrap_err();
    assert!(err.contains("occurs 2 times"), "{err}");
    // replace_all still replaces the matches that do not overlap
    assert_eq!(apply::replace("aaa", "aa", "b", true, "f").unwrap(), ("ba".to_string(), 1));
}

#[tokio::test]
async fn a_binary_file_is_refused_and_left_byte_identical() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("x.bin"), [1u8, 0, 2]).unwrap();
    let out = Edit.run(json!({"path": "x.bin", "old_string": "a", "new_string": "b"}), &c).await;
    assert_eq!(out.content, "edit: x.bin is a binary file");
    assert_eq!(std::fs::read(d.path().join("x.bin")).unwrap(), [1u8, 0, 2]);
}

#[test]
fn the_schema_names_the_keys_of_a_change_and_of_a_file_entry() {
    let p = Edit.spec().parameters;
    let change = &p["properties"]["edits"]["items"];
    assert_eq!(change["required"], json!(["old_string", "new_string"]));
    assert_eq!(change["properties"]["replace_all"]["type"], "boolean");
    let file = &p["properties"]["files"]["items"];
    assert_eq!(file["required"], json!(["path"]));
    assert_eq!(file["properties"]["edits"]["items"], *change, "a file's edits are the same entries");
    assert!(p.get("required").is_none(), "`path` is not required when `files` is used");
}

#[test]
fn a_small_window_gets_the_entries_as_bare_objects_that_still_have_items() {
    let mut spec = Edit.compact().unwrap().apply(Edit.spec());
    Edit.trim_compact(&mut spec);
    let props = &spec.parameters["properties"];
    assert_eq!(props["edits"]["items"], json!({"type": "object"}));
    let file = &props["files"]["items"];
    assert_eq!(file["properties"]["path"]["type"], "string");
    assert_eq!(file["properties"]["edits"]["items"], json!({"type": "object"}));
    // what the whole text adds to a request, the short one leaves out: the compact text stays near its old size
    let (short, whole) = (json!(spec).to_string().len(), json!(Edit.spec()).to_string().len());
    assert!(short < 800 && short < whole - 400, "short {short}, whole {whole}");
}

/// A crate in a folder of its own, outside the project folder of the context under test.
fn crate_outside(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    // A name of its own: crates of one name and version share a build under a shared CARGO_TARGET_DIR.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = tempfile::tempdir().unwrap();
    let name = format!("o{n}_{}", std::process::id());
    let manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    std::fs::write(d.path().join("Cargo.toml"), manifest).unwrap();
    std::fs::create_dir(d.path().join("src")).unwrap();
    let f = d.path().join("src/lib.rs");
    std::fs::write(&f, body).unwrap();
    (d, f)
}

#[tokio::test]
async fn a_data_file_is_checked_even_when_the_step_check_runs() {
    let _on = crate::edit_check::SWITCH.lock().await;
    let d = tempfile::tempdir().unwrap();
    let mut c = ToolContext::new(d.path().to_path_buf());
    c.post_check = true;
    std::fs::write(d.path().join("config.json"), "{\n  \"a\": 1\n}\n").unwrap();
    let out = Edit.run(json!({"path": "config.json", "old_string": "\"a\": 1", "new_string": "\"a\": ,"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("\ncheck: config.json | invalid JSON at line 2"), "{}", out.content);
    // a source file inside the folder is left to the step check
    std::fs::write(d.path().join("a.py"), "x = 1\n").unwrap();
    let out = Edit.run(json!({"path": "a.py", "old_string": "x = 1", "new_string": "x = = 1"}), &c).await;
    assert!(!out.content.contains("check:") && !out.content.contains("diagnostics:"), "{}", out.content);
}

#[tokio::test]
async fn a_file_outside_cwd_is_checked_even_when_the_step_check_runs() {
    if !crate::checkers::program_ok("cargo") {
        return;
    }
    let _on = crate::edit_check::SWITCH.lock().await;
    let (_o, f) = crate_outside("pub fn a() -> u32 {\n    1\n}\n");
    // build it once with room to spare, so the edit's own check is the quick one a real session gets
    assert_eq!(crate::edit_check::report_within(&f, std::time::Duration::from_secs(120)).await, "");
    let cwd = tempfile::tempdir().unwrap();
    let mut c = ToolContext::new(cwd.path().to_path_buf());
    c.post_check = true;
    let args = json!({"path": f.to_string_lossy(), "old_string": "    1", "new_string": "    \"no\""});
    let out = Edit.run(args, &c).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("\ndiagnostics:\n") && out.content.contains("error[E0308]"), "{}", out.content);
}

#[tokio::test]
async fn a_miss_across_mixed_line_ends_names_each_lines_end() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("f.txt"), "x\r\na\r\nb\nc\n").unwrap();
    let out = Edit.run(json!({"path": "f.txt", "old_string": "a\nb\nc", "new_string": "z"}), &c).await;
    assert!(out.is_error);
    assert!(out.content.contains("lines 2-4 match it except for line ends"), "{}", out.content);
    assert!(out.content.contains("line 2 CRLF, lines 3-4 LF"), "{}", out.content);
    // copying the shown lines would miss again, so the text does not tell to copy them
    assert!(!out.content.contains("Copy this text"), "{}", out.content);
    assert!(out.content.contains("`old_string` must end its lines the same way"), "{}", out.content);
    assert!(!out.content.contains("spaces or tabs"), "{}", out.content);
    assert_eq!(std::fs::read(d.path().join("f.txt")).unwrap(), b"x\r\na\r\nb\nc\n", "nothing written");
}

#[tokio::test]
async fn a_failed_later_change_says_its_line_numbers_count_the_text_after_the_earlier_ones() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("f.txt"), "a\nb\nc\n").unwrap();
    let edits = json!([
        {"old_string": "a", "new_string": "n1\nn2\nn3\nn4\nn5\na"},
        {"old_string": "  b", "new_string": "z"}
    ]);
    let out = Edit.run(json!({"path": "f.txt", "edits": edits}), &c).await;
    assert!(out.is_error);
    assert!(out.content.contains("change 2 of 2"), "{}", out.content);
    assert!(out.content.contains("line 7 matches it"), "{}", out.content);
    assert!(out.content.contains("changes 1-1 left it in memory"), "{}", out.content);
    assert!(out.content.contains("not the file on disk"), "{}", out.content);
    assert_eq!(std::fs::read_to_string(d.path().join("f.txt")).unwrap(), "a\nb\nc\n", "nothing written");
    // the first change failing has no earlier change to blame
    let edits = json!([{"old_string": "  b", "new_string": "z"}, {"old_string": "c", "new_string": "d"}]);
    let out = Edit.run(json!({"path": "f.txt", "edits": edits}), &c).await;
    assert!(out.is_error && !out.content.contains("in memory"), "{}", out.content);
}

#[tokio::test]
async fn edit_reports_new_marks() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    std::fs::write(d.path().join("a.txt"), "abc").unwrap();
    let out = Edit.run(json!({"path":"a.txt", "old_string":"abc", "new_string":"a—b"}), &c).await;
    assert!(out.content.contains("new marks of machine-written text:"), "{}", out.content);
}

#[tokio::test]
async fn edit_strips_only_added_invisibles_and_keeps_existing_joiners() {
    let d = tempfile::tempdir().unwrap();
    let c = ToolContext::new(d.path().to_path_buf());
    let path = d.path().join("a.txt");
    std::fs::write(&path, "old\u{200b} 👩\u{200d}💻 end").unwrap();
    let out = Edit.run(json!({"path":"a.txt", "old_string":"end", "new_string":"new\u{2060} 👨\u{200d}💻"}), &c).await;
    assert!(out.content.contains("removed newly added invisible characters"), "{}", out.content);
    assert!(out.content.lines().next().unwrap().contains(": +"), "{}", out.content);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "old\u{200b} 👩\u{200d}💻 new 👨\u{200d}💻");
}
