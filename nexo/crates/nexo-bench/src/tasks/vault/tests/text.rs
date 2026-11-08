use vault::text::*;

#[test]
fn names_are_reduced_to_lowercase_words() {
    assert_eq!(sanitize_name("My Report!"), "my-report");
    assert_eq!(sanitize_name("../etc/passwd"), "etc-passwd");
    assert_eq!(sanitize_name("  Q3 -- plan (v2) "), "q3-plan-v2");
    assert_eq!(sanitize_name(""), "untitled");
    assert_eq!(sanitize_name("***"), "untitled");
}

#[test]
fn long_names_are_cut_without_a_trailing_dash() {
    let long = "word ".repeat(40);
    let name = sanitize_name(&long);
    assert!(name.len() <= MAX_NAME);
    assert!(!name.ends_with('-') && !name.starts_with('-'));
    assert!(name.starts_with("word-word"));
}

#[test]
fn user_names_follow_the_rules() {
    assert!(is_valid_username("alice_01"));
    assert!(!is_valid_username("al"));
    assert!(!is_valid_username("a b c"));
    assert!(!is_valid_username(&"x".repeat(25)));
}

#[test]
fn truncate_marks_the_cut() {
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("0123456789abc", 10), "0123456...");
    assert_eq!(truncate("ñandú ñandú ñandú", 8), "ñandú...");
}

#[test]
fn display_text_is_escaped() {
    assert_eq!(escape_display("<b>\"R&D\"</b>'"), "&lt;b&gt;&quot;R&amp;D&quot;&lt;/b&gt;&#39;");
}

#[test]
fn tags_are_sanitized_and_deduplicated() {
    assert_eq!(parse_tags("Work, work ,Q3 Plan,,"), ["work", "q3-plan"]);
    assert_eq!(parse_tags("a,b,c,d,e,f,g,h,i,j").len(), MAX_TAGS);
    assert!(parse_tags("").is_empty());
}

#[test]
fn fields_always_come_back_in_the_asked_number() {
    assert_eq!(split_fields("a | b | c", 3), ["a", "b", "c"]);
    assert_eq!(split_fields("a", 3), ["a", "", ""]);
    assert_eq!(split_fields("a|b|c|d", 2), ["a", "b|c|d"]);
    assert!(has_control("a\nb") && !has_control("a b"));
}
