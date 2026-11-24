//! Tests of the markdown renderer.

use super::*;

#[test]
fn helpers_style_marks_and_leave_block_syntax_alone() {
    let t = Theme::dark();
    let runs = inline_runs("run `nexo login` now", &t, Style::default());
    let joined: String = runs.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(joined, "run nexo login now");
    assert!(runs.len() >= 3, "{runs:?}");
    assert_eq!(inline_runs("  - a `b`", &t, Style::default()), vec![("  - a `b`".to_string(), Style::default())]);
    assert_eq!(strip_md("**bold** `code` __x__"), "bold code x");
    assert!(looks_markdown("## Report\ntext"));
    assert!(looks_markdown("| a | b |\n|---|---|\n| 1 | 2 |"));
    assert!(!looks_markdown("#!/bin/sh\nls -la\n# comment"));
    let l = md_lines("a **b** c", 20, &t, Style::default(), false, &[Span::raw("> ")]);
    assert_eq!(text_of(&l[0]), "> a b c");
}

fn texts(md: &str, w: usize) -> Vec<String> {
    render(md, w, &Theme::dark()).iter().map(text_of).collect()
}

#[test]
fn headings_lists_and_paragraphs() {
    let t =
        texts("# Title\n\nSome *emphasis* and **bold** text.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n", 40);
    assert_eq!(t[0], "Title");
    assert_eq!(t[1], "");
    assert_eq!(t[2], "Some emphasis and bold text.");
    assert!(t.contains(&"• one".to_string()));
    assert!(t.contains(&"  ◦ nested".to_string()));
    assert!(t.contains(&"1. first".to_string()));
    assert!(t.contains(&"2. second".to_string()));
}

#[test]
fn emphasis_is_a_colour_not_italics_and_strong_text_stands_out() {
    let style_of = |theme: &Theme, word: &str| {
        render("plain *soft* and **loud**", 40, theme)
            .iter()
            .flat_map(|l| l.spans.clone())
            .find(|s| s.content.contains(word))
            .unwrap()
            .style
    };
    let t = Theme::dark();
    for theme in [Theme::dark(), Theme::light(), Theme::mono()] {
        for w in ["plain", "soft", "loud"] {
            assert!(!style_of(&theme, w).add_modifier.contains(Modifier::ITALIC), "{} {w}", theme.name);
        }
    }
    assert_eq!(style_of(&t, "soft").fg, Some(t.info));
    assert_eq!(style_of(&t, "loud").fg, Some(t.accent2));
    assert!(style_of(&t, "loud").add_modifier.contains(Modifier::BOLD));
    assert!(style_of(&Theme::mono(), "soft").add_modifier.contains(Modifier::BOLD));
}

#[test]
fn task_items_show_a_checkbox_in_place_of_the_bullet() {
    let t = texts("- [ ] open\n- [x] done\n  - plain\n- after", 40);
    assert_eq!(t[0], "☐ open");
    assert_eq!(t[1], "☑ done");
    assert_eq!(t[2], "  ◦ plain");
    assert_eq!(t[3], "• after");
}

#[test]
fn code_fences_carry_a_language_label_and_keep_lines() {
    let t = texts("Before\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nAfter", 40);
    let i = t.iter().position(|l| l.starts_with("rust ──")).expect("label");
    assert_eq!(t[i + 1].trim_end(), "▎ fn main() {");
    assert_eq!(t[i + 2].trim_end(), "▎     let x = 1;");
    assert_eq!(t[i + 3].trim_end(), "▎ }");
    assert_eq!(t.last().unwrap(), "After");
}

#[test]
fn numbered_lists_align_and_inline_code_keeps_its_text() {
    let md = (1..=10).map(|i| format!("{i}. item")).collect::<Vec<_>>().join("\n");
    let t = texts(&md, 40);
    assert_eq!(t[0], " 1. item");
    assert_eq!(t[9], "10. item");
    let t = texts("use `x` here", 40);
    assert_eq!(t[0], "use x here");
}

#[test]
fn tables_align_columns() {
    let t = texts("| a | bb |\n|---|---|\n| 1 | 2 |\n", 40);
    assert_eq!(t[0], "a │ bb");
    assert_eq!(t[1], "──┼───");
    assert_eq!(t[2], "1 │ 2 ");
}

#[test]
fn long_cells_wrap_inside_their_column_and_numbers_keep_their_alignment() {
    let t =
        texts("| name | note | $ |\n|---|---|---:|\n| a | one two three four five six | 4.99 |\n| b | x | 10 |\n", 24);
    assert!(t.iter().all(|l| width(l) <= 24), "{t:?}");
    let all = t.join("\n");
    assert!(all.contains("six") && !all.contains('…'), "nothing is cut: {t:?}");
    assert!(t.iter().any(|l| l.ends_with("│ 4.99")) && t.iter().any(|l| l.ends_with("│   10")), "{t:?}");
    assert!(t.iter().any(|l| l.contains('╌')), "wrapped rows are kept apart: {t:?}");
}

#[test]
fn quotes_links_and_long_lines_wrap() {
    let t = texts("> quoted text here\n\nsee [docs](https://example.com) now", 14);
    assert!(t[0].starts_with("▎ quoted"));
    assert!(t.iter().any(|l| l.contains("docs")));
    assert!(t.iter().all(|l| width(l) <= 14), "{t:?}");
}

#[test]
fn only_exact_url_labels_omit_the_address_suffix() {
    let url = "https://one.example";
    for label in [url.to_string(), "https://**one**.example".to_string()] {
        assert_eq!(texts(&format!("[{label}]({url})"), 120), vec![url.to_string()]);
    }
    for label in [format!("read {url}"), format!("read **{url}** now"), format!("{url}/more")] {
        let visible = label.replace("**", "");
        assert_eq!(texts(&format!("[{label}]({url})"), 120), vec![format!("{visible} ({url})")]);
    }
    assert_eq!(
        texts(&format!("[read {url}]({url})[second](https://two.example)"), 120),
        vec![format!("read {url} ({url})second (https://two.example)")]
    );
}

#[test]
fn a_long_code_line_always_ends_in_an_ellipsis_whatever_the_width() {
    let md = "```rust\nlet value = compute(first_argument, second_argument, third_argument);\n```";
    for w in 10..40 {
        let t = texts(md, w);
        let row = t.iter().find(|l| l.starts_with('▎')).expect("a code row");
        assert!(width(row) <= w, "{w}: {row:?}");
        assert!(row.trim_end().ends_with('…'), "cut without a mark at {w}: {row:?}");
    }
}

#[test]
fn a_link_in_a_table_cell_stays_in_its_cell() {
    let t = texts("| name | site |\n|---|---|\n| a | [docs](https://example.com/very/long/path) |\n\nafter", 40);
    let all = t.join("\n");
    assert!(t.iter().all(|l| width(l) <= 40), "{all}");
    let n = t.len();
    assert_eq!(t[n - 1], "after", "{all}");
    assert!(t[n - 2].is_empty() && t[n - 3].contains('│'), "no URL line left behind the table: {all}");
    assert!(all.contains("docs") && all.contains("example.com"), "{all}");
}

#[test]
fn an_unclosed_fence_while_streaming_still_renders() {
    let t = texts("text\n\n```py\nprint(1)", 30);
    assert!(t.iter().any(|l| l.starts_with("py ")));
    assert!(t.iter().any(|l| l.contains("print(1)")));
}

/// The screen at `cols`x`rows` with a long answer ending in a Rust fence: the row of the fence label.
fn label_row(cols: u16, rows: u16) -> Vec<char> {
    use nexo_core::agent::AgentEvent;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let opts = crate::TuiOptions {
        cwd: std::path::PathBuf::from("/tmp/project"),
        provider: "deepseek".into(),
        model: "deepseek-chat".into(),
        models: vec![("deepseek".into(), "deepseek-chat".into())],
        providers: vec![],
        theme: "nexo".into(),
        mouse: true,
        show_reasoning: true,
        timestamps: false,
        fold_after: 4,
        group_tools: true,
        complete_words: true,
        suggest: false,
        hints: true,
        keys: Default::default(),
        notify_after: 15,
        animations: false,
        figure: nexo_core::config::Figure::Galaxy,
        home_stars: true,
        bursts: true,
        detail: nexo_core::config::Detail::Full,
        todo_strip: false,
        status_line: vec![],
        status_command: String::new(),
        vim: false,
        prompt: None,
        session_id: None,
        sessions: vec![],
        skills: vec![],
    };
    let mut a = crate::App::new(opts, tx);
    let lead: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let text = format!("{}\n\n```rust\nfn main() {{}}\n```\n", lead.join("\n\n"));
    a.handle_agent(AgentEvent::TextDelta { text });
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(cols, rows)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut a)).unwrap();
    let rows = crate::ui::rows_of(term.backend().buffer());
    let row = rows.iter().find(|r| r.trim_end().ends_with("rust") || r.contains("rust ")).expect("the label row");
    row.chars().collect()
}

/// The rule after the label stops one empty column short of the scrollbar's.
fn assert_gap(cols: u16, rows: u16) {
    let row = label_row(cols, rows);
    let n = cols as usize;
    assert_eq!(row.len(), n, "{row:?}");
    let rule_end = row.iter().rposition(|c| *c == '─').expect("rule");
    assert_eq!(rule_end, n - 3, "the rule ends two columns before the edge: {row:?}");
    assert_eq!(row[n - 2], ' ', "{row:?}");
    assert_ne!(row[n - 1], ' ', "the scrollbar is drawn: {row:?}");
}

#[test]
fn the_code_label_leaves_a_gap_before_the_scrollbar_at_80x24() {
    assert_gap(80, 24);
}

#[test]
fn the_code_label_leaves_a_gap_before_the_scrollbar_at_40x12() {
    assert_gap(40, 12);
}

#[test]
fn a_table_too_wide_for_the_screen_is_drawn_as_head_value_lines() {
    let heads: Vec<String> = (1..=12).map(|i| format!("col{i:02}")).collect();
    let row = |n: usize| (1..=12).map(|i| format!("value{i:02}{}", if n == 2 { "b" } else { "" })).collect::<Vec<_>>();
    let line = |cells: &[String]| format!("| {} |", cells.join(" | "));
    let md = format!("{}\n|{}\n{}\n{}\n", line(&heads), "---|".repeat(12), line(&row(1)), line(&row(2)));
    let (lines, meta) = render_as_meta(&md, 60, &Theme::dark(), false);
    let texts: Vec<String> = lines.iter().map(text_of).collect();
    for t in &texts {
        assert!(width(t) <= 60, "{t:?} is {} columns", width(t));
    }
    assert!(texts.iter().any(|t| t.trim_end() == "col12: value12"), "{texts:?}");
    let region = meta.regions.iter().find(|r| r.kind == crate::regions::Kind::Table).expect("the table region");
    let src = region.src.as_ref().expect("the table keeps its copy text");
    assert_eq!(src.len(), (region.end - region.start + 1) as usize);
    // the copy path drops the rows a region marks as skipped and joins the rest by a newline
    let copy: Vec<&str> = src
        .iter()
        .enumerate()
        .filter(|(k, _)| !region.skip.contains(&(region.start + *k as u32)))
        .map(|(_, s)| s.as_str())
        .collect();
    let copy = copy.join("\n");
    assert!(copy.starts_with("col01\tcol02"), "{copy:?}");
    assert!(copy.contains("value01\tvalue02"), "{copy:?}");
    assert!(copy.contains("value01b\tvalue02b"), "{copy:?}");
}
