use super::*;
use serde_json::json;

#[test]
fn words_and_terms_follow_the_curator() {
    let mut ws = vec![];
    words("cfg.epoch_tokens json.dumps v2 src/parser.rs 2024 1999x 3.11.2 parseLine main.rs api.example.com", &mut ws);
    for w in ["epoch_tokens", "src/parser.rs", "3.11.2", "parseLine", "main.rs", "api.example.com"] {
        assert!(ws.contains(&w), "{w}: {ws:?}");
    }
    assert!(!ws.contains(&"2024") && !ws.contains(&"json.dumps"), "{ws:?}");
    let mut lex = Lex::default();
    let (t, _) = lex.analyze("Fix parseLine in the handler_config");
    let mut got: Vec<&str> = t.iter().map(|&k| lex.strs[k as usize].as_str()).collect();
    got.sort_unstable();
    assert_eq!(got, vec!["confi", "fix", "handl", "handler_config", "line", "parse", "parseLine"]);
}

#[test]
fn line_numbers_are_recognized() {
    assert_eq!(strip_brief_lineno("     8│ fn x()"), "fn x()");
    assert_eq!(strip_brief_lineno("  12\t\tfoo"), "foo");
    assert_eq!(strip_brief_lineno("  12\t \tfoo"), "foo");
    assert_eq!(strip_brief_lineno("12 | bar"), "bar");
    assert_eq!(strip_brief_lineno("12 bar"), "12 bar");
    assert_eq!(strip_mechanical_lineno("  12→x"), "x");
    assert_eq!(strip_mechanical_lineno("12: x"), "x");
    assert_eq!(strip_mechanical_lineno("12 | x"), "x");
    assert_eq!(strip_mechanical_lineno("12x"), "12x");
}

#[test]
fn paragraphs_cut_at_blank_lines() {
    assert_eq!(split_paragraphs("a\nb\nc\n\nd\ne\nf\ng"), vec![(0, 4), (4, 8)]);
    assert_eq!(split_paragraphs("a\nb\nc\n\nd"), vec![(0, 5)]);
    assert_eq!(split_paragraphs(""), vec![(0, 1)]);
}

#[test]
fn tokens_are_estimated_on_canonical_json() {
    let b = json!({"type": "text", "text": "héllo \"x\"\n", "cache_control": {"type": "ephemeral"}});
    assert_eq!(stripped_len(&b, &mut 0), r#"{"text":"héllo \"x\"\n","type":"text"}"#.chars().count());
    let img = json!({"type": "image", "source": {"type": "base64", "data": "AAAA"}});
    let mut media = 0;
    assert_eq!(stripped_len(&img, &mut media), r#"{"source":{"type":"base64"},"type":"image"}"#.len());
    assert_eq!(media, 1);
    assert_eq!(json_len(&json!({"a": [1, 2.5, 1e-5, null, true]})), r#"{"a":[1,2.5,1e-05,null,true]}"#.len());
}

#[test]
fn a_selection_bears_on_the_task_and_keeps_its_budget() {
    let lines: Vec<String> =
        (0..60).map(|i| format!("{:>6}│ fn parse_line_{i}(s: &str) -> Token {{ lex(s, {i}) }}", i + 1)).collect();
    let msgs = vec![
        json!({"role": "user", "content": "Fix parse_line_7 in src/parser.rs"}),
        json!({"role": "assistant", "content": [{"type": "tool_use", "id": "r1", "name": "read", "input": {"path": "src/parser.rs"}}]}),
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "r1", "content": lines.join("\n")}]}),
    ];
    let r = select("Fix parse_line_7 in src/parser.rs", &msgs, 600);
    let read = r.spans.iter().find(|s| s.label == "read src/parser.rs").expect("the read");
    assert!(read.text.contains("fn parse_line_7("), "{}", read.text);
    assert!(read.text.lines().count() <= LONG, "{}", read.text);
    assert!(r.spans.iter().map(|s| s.tokens).sum::<usize>() <= 600);
    assert_eq!(r, select("Fix parse_line_7 in src/parser.rs", &msgs, 600));
    assert!(select("", &msgs, 600).spans.is_empty());
}
