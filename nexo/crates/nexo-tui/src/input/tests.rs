use super::*;

#[test]
fn a_short_paste_and_a_recalled_draft_are_clean_and_a_long_paste_keeps_its_tabs() {
    let mut i = Input::new("");
    i.insert_paste("a\tb\x1b[2Jc\x07\nd");
    assert_eq!(i.text(), "a    bc\nd", "escapes and controls out, tabs as spaces, line breaks kept");
    let long: String = (1..=40).map(|n| format!("\x1b[31mline\t{n}\n")).collect();
    i.clear();
    i.insert_paste(&long);
    assert!(!i.full_text().contains('\x1b'), "{:?}", i.full_text());
    assert!(i.full_text().contains('\t'), "a long paste is sent as pasted: {:?}", i.full_text());
    i.set_text("x\x1b]0;title\x07y\tz");
    assert_eq!(i.text(), "xy    z");
    let (rows, _) = wrapped_rows(&i, 20);
    assert!(rows.iter().all(|r| !r.chars().any(char::is_control)), "{rows:?}");
}

#[test]
fn a_long_paste_keeps_joiners_selectors_and_bidi_marks() {
    let paste: String = (1..=40).map(|_| "👩\u{200D}💻 ❤\u{FE0F} \u{202B}abc\t1\n").collect();
    let mut i = Input::new("");
    i.insert_paste(&paste);
    assert_eq!(i.chip_labels().len(), 1, "a long paste is a chip");
    assert_eq!(i.full_text(), paste);
    let (rows, _) = wrapped_rows(&i, 40);
    assert!(rows.iter().all(|r| !r.chars().any(char::is_control)), "{rows:?}");
}

#[test]
fn a_recalled_emoji_request_stays_editable_text() {
    let mut i = Input::new("");
    i.set_text("❤\u{FE0F} ok");
    assert!(i.chip_labels().is_empty());
    assert_eq!(i.text(), i.full_text());
}

#[test]
fn a_combining_mark_takes_no_column() {
    // "e" + U+0301 is one cell on screen: the caret after it sits at column 1, not 2
    let mut i = Input::new("");
    i.set_text("e\u{301}x");
    let (rows, cursor) = wrapped_rows(&i, 20);
    assert_eq!((rows.len(), cursor), (1, (0, 2)));
    // the row of two cells is not cut early in a box two cells wide
    let (rows, _) = wrapped_rows(&i, 2);
    assert_eq!(rows[0], "e\u{301}x");
    // a click on the second cell lands before the "x", past the mark
    assert_eq!(cell_at(&i, 20, 0, 1), (0, 2));
}

#[test]
fn a_wide_character_in_a_one_cell_box_still_lays_out() {
    // QA 13:2x: it looped forever (a 2-cell character never fits 1 cell), growing rows until the OOM killer
    let mut i = Input::new("");
    i.insert_paste("宽x宽");
    for width in 0..4 {
        let (rows, _) = wrapped_rows(&i, width);
        assert!(rows.len() <= 4, "width {width}: {rows:?}");
    }
}

#[test]
fn a_long_paste_is_folded_in_the_draft_and_sent_whole() {
    let mut i = Input::new("");
    i.insert_paste("one line");
    assert_eq!(i.text(), "one line", "a short paste goes in as typed");
    let long: String = (1..=40).map(|n| format!("line {n}\r\n")).collect();
    i.insert_paste(" ");
    i.insert_paste(&long);
    i.insert_paste(" explain it");
    assert_eq!(i.text(), "one line [Pasted text #1 +40 lines] explain it");
    let full = i.full_text();
    assert!(full.starts_with("one line line 1\nline 2\n") && full.ends_with("line 40\n explain it"), "{full}");
    assert!(!full.contains('\r'));
    let wide = "x".repeat(2000);
    i.insert_paste(&wide);
    assert!(i.text().ends_with("[Pasted text #2 2000 chars]"), "{}", i.text());
    assert!(i.full_text().ends_with(&wide));
    i.clear();
    i.insert_paste(&long);
    assert_eq!(i.text(), "[Pasted text #1 +40 lines]", "numbering starts again with a new draft");
}

#[test]
fn a_paste_that_contains_a_later_chip_label_is_sent_as_typed() {
    let mut i = Input::new("");
    let first = format!("log line\n{}\n{}", "[Pasted text #2 2000 chars]", "x\n".repeat(30));
    let second = "y".repeat(2000);
    i.insert_paste(&first);
    i.insert_paste(" ");
    i.insert_paste(&second);
    assert_eq!(i.text(), "[Pasted text #1 +32 lines] [Pasted text #2 2000 chars]");
    assert_eq!(i.full_text(), format!("{first} {second}"));
}

fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, m)
}

#[test]
fn the_counter_shows_for_multiline_or_long_drafts_only() {
    let mut i = Input::new("");
    i.set_text("short");
    assert_eq!(counter(&i), None);
    i.set_text("a\nbc");
    assert_eq!(counter(&i).as_deref(), Some("2 lines · 4 chars"));
    i.set_text(&"x".repeat(250));
    assert_eq!(counter(&i).as_deref(), Some("1 line · 250 chars"));
}

#[test]
fn long_lines_wrap_to_the_width_and_the_cursor_follows() {
    let mut i = Input::new("");
    i.set_text("abcdefgh\nxy");
    let (rows, cur) = wrapped_rows(&i, 3);
    assert_eq!(rows, ["abc", "def", "gh", "xy"]);
    assert_eq!(cur, (3, 2));
    i.set_text("abc");
    let (rows, cur) = wrapped_rows(&i, 3);
    assert_eq!(rows, ["abc", ""]);
    assert_eq!(cur, (1, 0));
}

#[test]
fn a_wide_character_in_a_one_cell_box_keeps_the_cursor_row() {
    let mut i = Input::new("");
    i.set_text("宽a");
    let (rows, _) = wrapped_rows(&i, 1);
    assert_eq!(&rows[..2], ["宽", "a"], "the cursor after the last cell opens a row");
}

#[test]
fn readline_ctrl_u_k_w_delete_to_the_start_to_the_end_and_a_word() {
    let mut i = Input::new("");
    i.set_text("one two three");
    i.key(k(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "one two ");
    i.key(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "");
    // on a multi-line draft it cuts only back to the start of the cursor line
    i.set_text("a\nb\nc");
    i.key(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "a\nb\n");
    i.set_text("a\nbc\nd");
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    i.key(k(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "a\nc\nd");
    i.set_text("abc def");
    i.key(k(KeyCode::Char('a'), KeyModifiers::CONTROL));
    i.key(k(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "");
}

#[test]
fn a_click_and_a_drag_in_the_box_select_text_to_copy() {
    let mut i = Input::new("");
    i.set_text("hello world\nsecond");
    let a = cell_at(&i, 20, 0, 6);
    assert_eq!(a, (0, 6));
    i.press_at(a);
    i.drag_to(cell_at(&i, 20, 1, 3));
    assert_eq!(i.selected_text().as_deref(), Some("world\nseco"), "the cell under the mouse is in");
    assert_eq!(i.release_selection().as_deref(), Some("world\nseco"));
    assert_eq!(i.selected_text(), None);
    // past the end of a row the cell is the row's end; a wrapped row counts from its start
    assert_eq!(cell_at(&i, 20, 0, 19), (0, 11));
    assert_eq!(cell_at(&i, 4, 1, 1), (0, 5));
}

#[test]
fn up_down_home_end_follow_the_rows_of_the_box() {
    let mut i = Input::new("");
    i.wrap_width = 5;
    i.set_text("aaaa bbbb cccc");
    // rows: "aaaa ", "bbbb ", "cccc"; the cursor is at the end, in the last row
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 9), "up goes to the row above, same column");
    i.key(k(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 5), "home: start of the row");
    i.key(k(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 9), "end of a wrapped row: before its break");
    i.key(k(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor().0, 0);
    assert!(i.editor.cursor().1 >= 10);
}

#[test]
fn what_a_drag_paints_is_what_it_copies() {
    let mut i = Input::new("");
    i.set_text("hello world\nsecond");
    i.press_at((0, 6));
    i.drag_to((1, 2));
    // the cells the box paints, read from the draft, are the text that is copied
    let ((r0, c0), (r1, c1)) = i.selection_cells().unwrap();
    let lines = i.editor.lines();
    let painted: Vec<String> = (r0..=r1)
        .map(|r| {
            let cs: Vec<char> = lines[r].chars().collect();
            let (a, b) = (if r == r0 { c0 } else { 0 }, if r == r1 { c1 } else { cs.len() });
            cs[a..b].iter().collect()
        })
        .collect();
    assert_eq!(painted.join("\n"), i.selected_text().unwrap());
    assert_eq!(i.selected_text().as_deref(), Some("world\nsec"));
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_the_line() {
    let mut i = Input::new("");
    i.set_text("foo bar_1, baz");
    i.select_word((0, 5));
    assert_eq!(i.selected_text().as_deref(), Some("bar_1"));
    i.select_word((0, 3));
    assert_eq!(i.selected_text().as_deref(), Some(" "));
    i.select_word((0, 99));
    assert_eq!(i.selected_text().as_deref(), Some("baz"));
    i.select_line(0);
    assert_eq!(i.selected_text().as_deref(), Some("foo bar_1, baz"));
}

#[test]
fn a_single_click_copies_nothing() {
    let mut i = Input::new("");
    i.set_text("foo bar_1, baz");
    i.press_at((0, 8));
    assert_eq!(i.release_selection(), None, "a click without a drag copies nothing");
    assert_eq!(i.selected_text(), None, "and leaves no selection");
    assert_eq!(i.editor.cursor(), (0, 8), "the cursor is where the click was");
    // dragging back onto the same cell is still a click
    i.press_at((0, 8));
    i.drag_to((0, 8));
    assert_eq!(i.release_selection(), None);
    // a drag to another cell copies
    i.press_at((0, 4));
    i.drag_to((0, 6));
    assert_eq!(i.release_selection().as_deref(), Some("bar"));
}

#[test]
fn ctrl_z_undoes_an_edit_and_alt_z_redoes_it() {
    let mut i = Input::new("");
    i.set_text("hello");
    i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
    assert_eq!(i.text(), "hello!");
    i.key(k(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(i.text(), "hello");
    i.key(k(KeyCode::Char('z'), KeyModifiers::ALT));
    assert_eq!(i.text(), "hello!");
}

#[test]
fn enter_submits_and_ctrl_j_adds_a_line() {
    let mut i = Input::new("");
    for c in "hi".chars() {
        i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(i.key(k(KeyCode::Char('j'), KeyModifiers::CONTROL)), Edit::Changed);
    assert_eq!(i.line_count(), 2);
    assert_eq!(i.key(k(KeyCode::Enter, KeyModifiers::NONE)), Edit::Submit);
    assert_eq!(i.text(), "hi\n");
}

#[test]
fn history_browses_up_and_down_and_keeps_the_draft() {
    let mut i = Input::new("");
    i.push_history("first");
    i.push_history("second");
    i.set_text("draft");
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(i.text(), "second");
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(i.text(), "first");
    i.key(k(KeyCode::Down, KeyModifiers::NONE));
    i.key(k(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(i.text(), "draft");
}

#[test]
fn editing_a_recalled_entry_keeps_the_stash_and_the_browse_position() {
    let mut i = Input::new("");
    i.push_history("first");
    i.push_history("second");
    i.set_text("half-written message");
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(i.text(), "second");
    i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
    i.key(k(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(i.text(), "first", "Up continues from the browsed index");
    i.key(k(KeyCode::Down, KeyModifiers::NONE));
    i.key(k(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(i.text(), "half-written message");
}
#[test]
fn replaces_the_word_at_the_cursor() {
    let mut i = Input::new("");
    i.set_text("see @src/ma");
    i.replace_range(5, "src/main.rs ");
    assert_eq!(i.text(), "see @src/main.rs ");
    assert_eq!(i.cursor_offset(), i.text().len());
}

#[test]
fn the_vim_keys_move_edit_and_switch_modes() {
    let mut i = Input::new("");
    i.vim = Some(VimMode::Insert);
    for c in "hello world".chars() {
        i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert!(i.vim_escape(), "Esc leaves insert mode");
    assert_eq!(i.vim, Some(VimMode::Normal));
    let n = |i: &mut Input, c: char| i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    n(&mut i, '0');
    n(&mut i, 'x');
    assert_eq!(i.text(), "ello world");
    n(&mut i, 'w');
    n(&mut i, 'D');
    assert_eq!(i.text(), "ello ");
    n(&mut i, 'u');
    assert_eq!(i.text(), "ello world", "u undoes");
    n(&mut i, 'A');
    assert_eq!(i.vim, Some(VimMode::Insert));
    i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
    assert_eq!(i.text(), "ello world!");
    i.vim_escape();
    n(&mut i, 'o');
    i.key(k(KeyCode::Char('z'), KeyModifiers::NONE));
    i.vim_escape();
    assert_eq!(i.text(), "ello world!\nz");
    n(&mut i, 'd');
    n(&mut i, 'd');
    assert_eq!(i.text(), "ello world!", "dd deletes the line");
    assert_eq!(i.key(k(KeyCode::Enter, KeyModifiers::NONE)), Edit::Submit, "Enter sends from normal mode");
    assert!(!i.vim_escape(), "Esc in normal mode is the app's (interrupt, clear)");
}

const CHIP: &str = "[Pasted text #1 +12 lines]";

/// "ab" + a folded paste + "cd", the cursor at the end.
fn chip_draft() -> Input {
    let mut i = Input::new("");
    for c in "ab".chars() {
        i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    }
    i.insert_paste(&"x\n".repeat(12));
    for c in "cd".chars() {
        i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(i.text(), format!("ab{CHIP}cd"));
    i
}

fn at(i: &mut Input, col: usize) {
    i.editor.move_cursor(CursorMove::Jump(0, col as u16));
}

#[test]
fn backspace_at_the_end_of_a_chip_removes_it_whole() {
    let mut i = chip_draft();
    at(&mut i, 2 + CHIP.len());
    i.key(k(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(i.text(), "abcd");
    assert!(i.pastes.is_empty(), "its entry goes too");
    assert_eq!(i.full_text(), "abcd");
}

#[test]
fn delete_at_the_start_of_a_chip_removes_it_whole() {
    let mut i = chip_draft();
    at(&mut i, 2);
    i.key(k(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(i.text(), "abcd");
    assert!(i.pastes.is_empty());
    assert_eq!(i.editor.cursor(), (0, 2));
}

#[test]
fn ctrl_w_and_alt_backspace_remove_a_chip_whole() {
    for m in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        let code = if m == KeyModifiers::CONTROL { KeyCode::Char('w') } else { KeyCode::Backspace };
        let mut i = chip_draft();
        at(&mut i, 2 + CHIP.len());
        i.key(k(code, m));
        assert_eq!(i.text(), "abcd", "{m:?}");
        assert!(i.pastes.is_empty(), "{m:?}");
    }
}

#[test]
fn the_cursor_jumps_over_a_chip() {
    let mut i = chip_draft();
    let end = 2 + CHIP.len();
    at(&mut i, end);
    i.key(k(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 2), "Left from the end lands before the chip");
    i.key(k(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, end), "Right from the start lands after it");
    i.wrap_width = 80;
    i.key(k(KeyCode::Home, KeyModifiers::NONE));
    i.key(k(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, end + 2));
    // a click or drag inside the label goes to the nearest edge
    assert_eq!(cell_at(&i, 80, 0, 5), (0, 2));
    assert_eq!(cell_at(&i, 80, 0, end - 3), (0, end));
}

#[test]
fn a_new_chip_does_not_reuse_a_removed_number() {
    let mut i = chip_draft();
    i.insert_paste(&"y\n".repeat(12));
    assert!(i.text().ends_with("[Pasted text #2 +12 lines]"));
    at(&mut i, 2 + CHIP.len());
    i.key(k(KeyCode::Backspace, KeyModifiers::NONE));
    i.insert_paste(&"z\n".repeat(12));
    assert!(i.text().contains("[Pasted text #3 +12 lines]"), "{}", i.text());
    assert_eq!(i.pastes.len(), 2);
}

#[test]
fn a_joiner_selector_or_skin_tone_typed_on_its_own_is_left_out() {
    let mut i = Input::new("");
    for c in ['\u{2764}', '\u{FE0F}', 'a', '\u{1F44D}', '\u{1F3FD}', '\u{200D}', 'b'] {
        i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(i.text(), "\u{2764}a\u{1F44D}b", "the same text a paste of it leaves");
    let mut pasted = Input::new("");
    pasted.insert_paste("\u{2764}\u{FE0F}a\u{1F44D}\u{1F3FD}\u{200D}b");
    assert_eq!(pasted.text(), i.text());
}

#[test]
fn vim_x_takes_a_chip_or_a_letter_with_its_mark_whole() {
    let mut i = chip_draft();
    i.vim = Some(VimMode::Normal);
    at(&mut i, 2);
    i.key(k(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(i.text(), "abcd");
    assert!(i.pastes.is_empty(), "its entry goes too");
    let mut i = Input::new("");
    i.set_text("e\u{301}x");
    i.vim = Some(VimMode::Normal);
    at(&mut i, 0);
    i.key(k(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(i.text(), "x", "no lone combining mark is left");
}

#[test]
fn vim_motions_never_stop_inside_a_chip_or_a_flag() {
    let mut i = chip_draft();
    i.vim = Some(VimMode::Normal);
    at(&mut i, 2);
    i.key(k(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 2 + CHIP.len()), "l steps over the whole label");
    i.key(k(KeyCode::Char('h'), KeyModifiers::NONE));
    assert_eq!(i.editor.cursor(), (0, 2), "h steps back over it");
    let mut f = Input::new("");
    f.set_text("\u{1F1EB}\u{1F1F7}x");
    f.vim = Some(VimMode::Normal);
    at(&mut f, 0);
    f.key(k(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(f.editor.cursor(), (0, 2), "l steps over both letters of the flag");
}

#[test]
fn option_arrows_and_option_backspace_move_and_delete_by_word() {
    // Terminal.app and iTerm2 send these as Alt chords; they must reach the editor untouched
    let mut i = Input::new("");
    i.set_text("alpha beta gamma");
    if !cfg!(target_os = "macos") {
        // Linux and Windows keep Alt+arrows as they were: the editor has no rule for them
        i.key(k(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(i.cursor_offset(), "alpha beta gamma".len());
        return;
    }
    i.key(k(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(i.cursor_offset(), "alpha beta ".len());
    i.key(k(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(i.cursor_offset(), "alpha ".len());
    i.key(k(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(i.cursor_offset(), "alpha beta ".len(), "a word forward lands on the start of the next");
    i.key(k(KeyCode::Backspace, KeyModifiers::ALT));
    assert_eq!(i.text(), "alpha gamma");
}

#[test]
fn omega_redoes_on_a_mac_and_is_a_letter_elsewhere() {
    let mut i = Input::new("");
    i.set_text("hello");
    i.key(k(KeyCode::Char('!'), KeyModifiers::NONE));
    i.key(k(KeyCode::Char('z'), KeyModifiers::CONTROL));
    i.key(k(KeyCode::Char('Ω'), KeyModifiers::NONE));
    // with a redo waiting it redoes on a Mac; nothing else changes the text but the letter itself
    let expect = if cfg!(target_os = "macos") { "hello!" } else { "helloΩ" };
    assert_eq!(i.text(), expect);
    // with nothing to redo the character is typed
    i.key(k(KeyCode::Char('Ω'), KeyModifiers::NONE));
    assert!(i.text().ends_with('Ω'));
}

#[cfg(unix)]
#[test]
fn the_history_file_is_readable_by_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let d = tempfile::tempdir().unwrap();
    // a file left by an older run is tightened when the history is loaded
    let old = d.path().join("old.jsonl");
    std::fs::write(&old, "\"one\"\n").unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut i = Input::new("");
    i.persist_history(old.clone());
    i.push_history("two");
    assert_eq!(mode(&old), 0o600);
    assert_eq!(i.history(), ["one", "two"]);
    // a new file is private from the first line
    let fresh = d.path().join("fresh.jsonl");
    let mut j = Input::new("");
    j.persist_history(fresh.clone());
    j.push_history("secret");
    assert_eq!(mode(&fresh), 0o600);
    // trimming a long file rewrites it without widening the mode
    let long = d.path().join("long.jsonl");
    let lines: String = (0..2 * MAX_HISTORY + 5).map(|n| format!("\"p{n}\"\n")).collect();
    std::fs::write(&long, lines).unwrap();
    let mut k = Input::new("");
    k.persist_history(long.clone());
    assert_eq!(k.history().len(), MAX_HISTORY);
    assert_eq!(std::fs::read_to_string(&long).unwrap().lines().count(), MAX_HISTORY);
    assert_eq!(mode(&long), 0o600);
}

#[test]
fn a_vim_operator_pair_that_is_not_implemented_says_so_and_leaves_the_draft_alone() {
    let mut i = Input::new("");
    i.set_text("hello world");
    i.vim = Some(VimMode::Normal);
    let n = |i: &mut Input, c: char| i.key(k(KeyCode::Char(c), KeyModifiers::NONE));
    assert_eq!(n(&mut i, 'd'), Edit::Changed, "the operator waits for its second key");
    assert_eq!(n(&mut i, 'w'), Edit::Notice("vim: dw not supported".to_string()));
    assert_eq!(i.text(), "hello world");
    // the operator was cleared, so the next key is a plain one again
    assert_eq!(n(&mut i, 'q'), Edit::Ignored, "an unknown key with no operator stays silent, as in vim");
}
