use std::path::Path;

use vault::export::*;
use vault::store::Store;

fn store() -> Store {
    let mut s = Store::new();
    s.insert(1, "Q3 plan", "line one\nline \"two\"", vec!["work".into(), "q3".into()], 1);
    s.insert(1, "Q3 Plan!", "second", vec![], 2);
    s.insert(2, "Other", "x", vec![], 3);
    s
}

#[test]
fn formats_are_read_by_name() {
    assert_eq!(Format::parse("MD").unwrap(), Format::Markdown);
    assert_eq!(Format::parse(" txt ").unwrap(), Format::Text);
    assert_eq!(Format::parse("json").unwrap().extension(), "json");
    assert!(Format::parse("exe").is_err());
}

#[test]
fn json_strings_are_escaped() {
    assert_eq!(json_string("a\"b\\c\nd\te"), "\"a\\\"b\\\\c\\nd\\te\"");
    assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    assert_eq!(json_string("ñ"), "\"ñ\"");
}

#[test]
fn documents_render_in_each_format() {
    let s = store();
    let doc = s.get(1).unwrap();
    assert_eq!(render(doc, Format::Text), "Q3 plan\n\nline one\nline \"two\"\n");
    assert_eq!(render(doc, Format::Markdown), "# Q3 plan\n\nline one\nline \"two\"\n\n`work` `q3`\n");
    assert_eq!(
        render(doc, Format::Json),
        "{\"id\":1,\"owner\":1,\"title\":\"Q3 plan\",\"body\":\"line one\\nline \\\"two\\\"\",\"tags\":[\"work\",\"q3\"]}"
    );
}

#[test]
fn export_files_are_named_after_the_title() {
    let base = Path::new("/srv/exports");
    assert_eq!(export_path(base, "My Report!", Format::Markdown), base.join("my-report.md"));
    assert_eq!(export_path(base, "../../etc/passwd", Format::Text), base.join("etc-passwd.txt"));
    assert_eq!(export_path(base, "???", Format::Json), base.join("untitled.json"));
}

#[test]
fn two_titles_with_one_file_name_do_not_overwrite_each_other() {
    let s = store();
    let out = export_all(&s, 1, Path::new("out"), Format::Text);
    assert_eq!(out.len(), 2);
    assert_ne!(out[0].0, out[1].0);
    assert_eq!(out[0].0, Path::new("out/q3-plan.txt"));
    assert_eq!(export_all(&s, 2, Path::new("out"), Format::Text).len(), 1);
}
