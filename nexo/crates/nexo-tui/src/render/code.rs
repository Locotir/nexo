//! Syntax highlighting through syntect with bat's set of some two hundred languages, loaded once, theme-aware.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::theme::Theme;

fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(two_face::syntax::extra_newlines)
}

fn themes() -> &'static ThemeSet {
    static T: OnceLock<ThemeSet> = OnceLock::new();
    T.get_or_init(ThemeSet::load_defaults)
}

/// Expands tabs to four spaces.
pub fn detab(s: &str) -> String {
    if s.contains('\t') {
        s.replace('\t', "    ")
    } else {
        s.to_string()
    }
}

/// The lines of `code` highlighted for `lang` (a fence label or a file extension).
/// Without a known language, or on a mono theme, plain lines.
pub fn highlight(code: &str, lang: &str, theme: &Theme) -> Vec<Line<'static>> {
    let plain = || code.split('\n').map(|l| Line::from(detab(l))).collect::<Vec<_>>();
    let Some(theme_name) = theme.syntax else { return plain() };
    let ss = syntaxes();
    let Some(syntax) = syntax_for(lang, code) else { return plain() };
    let Some(st) = themes().themes.get(theme_name) else { return plain() };
    let mut h = HighlightLines::new(syntax, st);
    let mut out = Vec::new();
    for raw in code.split('\n') {
        let line = detab(raw);
        let mut spans = Vec::new();
        match h.highlight_line(&format!("{line}\n"), ss) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    let text = text.trim_end_matches('\n');
                    if text.is_empty() {
                        continue;
                    }
                    let mut s =
                        Style::default().fg(Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b));
                    if style.font_style.contains(FontStyle::BOLD) {
                        s = s.add_modifier(Modifier::BOLD);
                    }
                    spans.push(Span::styled(text.to_string(), s));
                }
            }
            Err(_) => spans.push(Span::raw(line.clone())),
        }
        out.push(Line::from(spans));
    }
    out
}

/// What `code` is when shown for what it means instead of as source: Markdown rendered,
/// JSON indented, CSV and TSV as an aligned table. None for anything else.
pub fn interpret(lang: &str, code: &str, width: usize, theme: &Theme) -> Option<Vec<Line<'static>>> {
    let name = syntax_for(lang, code)?.name.as_str();
    match name {
        "Markdown" => Some(super::markdown::render(code, width, theme)),
        "JSON" => {
            let v: serde_json::Value = serde_json::from_str(code).ok()?;
            let pretty = serde_json::to_string_pretty(&v).ok()?;
            Some(highlight(&pretty, "json", theme))
        }
        "Separated Values" | "Comma Separated Values" => Some(table(code, ',', width, theme)),
        "Tab Separated Values" => Some(table(code, '\t', width, theme)),
        _ => None,
    }
}

/// The lines of `code` interpreted when asked and possible, else highlighted; and whether
/// they were interpreted.
pub fn view(lang: &str, code: &str, width: usize, theme: &Theme, interpreted: bool) -> (Vec<Line<'static>>, bool) {
    match interpreted.then(|| interpret(lang, code, width, theme)).flatten() {
        Some(lines) => (lines, true),
        None => (highlight(code, lang, theme), false),
    }
}

/// The fields of one line of separated values; a field in double quotes may hold the separator.
fn fields(line: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == sep && !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Separated values as columns under a bold head, each column as wide as its widest
/// field and cut, the widest first, until the table fits `width`.
fn table(code: &str, sep: char, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    use super::wrap::{truncate, width as cols};
    let rows: Vec<Vec<String>> = code.lines().filter(|l| !l.trim().is_empty()).map(|l| fields(l, sep)).collect();
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut w: Vec<usize> =
        (0..n).map(|i| rows.iter().filter_map(|r| r.get(i)).map(|f| cols(f)).max().unwrap_or(0)).collect();
    let room = width.saturating_sub(3 * n.saturating_sub(1));
    while w.iter().sum::<usize>() > room && w.iter().any(|&x| x > 4) {
        let i = (0..n).max_by_key(|&i| w[i]).unwrap_or(0);
        w[i] -= 1;
    }
    let bar = theme.style(theme.border);
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut spans = Vec::new();
        for (i, cw) in w.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", bar));
            }
            let f = row.get(i).map(String::as_str).unwrap_or("");
            let f = if cols(f) > *cw { truncate(f, *cw) } else { f.to_string() };
            let pad = cw.saturating_sub(cols(&f));
            let numeric = f.trim().parse::<f64>().is_ok();
            let style = if r == 0 { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
            let cell =
                if numeric && r > 0 { format!("{}{f}", " ".repeat(pad)) } else { format!("{f}{}", " ".repeat(pad)) };
            spans.push(Span::styled(cell, style));
        }
        out.push(Line::from(spans));
        if r == 0 && rows.len() > 1 {
            let rule = w.iter().map(|cw| "─".repeat(*cw)).collect::<Vec<_>>().join("─┼─");
            out.push(Line::from(Span::styled(rule, bar)));
        }
    }
    out
}

/// Labels that name no language: the text stays plain.
const PLAIN: &[&str] = &["text", "txt", "plain", "plaintext", "output", "none", "nohighlight"];

/// The common names of languages that syntect knows by another one.
fn alias(lang: &str) -> &str {
    match lang {
        "sh" | "shell" | "zsh" | "ksh" | "console" | "shellsession" | "terminal" | "bash" => "bash",
        "c++" | "cxx" | "hpp" | "hh" => "cpp",
        "h" => "c",
        "c#" | "csharp" => "cs",
        "f#" | "fsharp" => "fs",
        "py3" | "python3" | "python" => "py",
        "golang" => "go",
        "rust" => "rs",
        "bat" | "batch" | "cmd" => "bat",
        "javascript" | "node" | "mjs" | "cjs" => "js",
        "typescript" => "ts",
        "yml" => "yaml",
        "docker" => "Dockerfile",
        "make" | "makefile" => "Makefile",
        "objc" | "objective-c" => "m",
        "kotlin" => "kt",
        "ruby" => "rb",
        "perl" => "pl",
        "markdown" | "md" => "md",
        "jsonc" | "json5" => "json",
        "patch" => "diff",
        "terraform" => "tf",
        other => other,
    }
}

/// The syntax for `lang` (a fence label, a file name or a path), else one told by the first
/// line of `code` (`#!/bin/sh`, `<?xml`, `<!DOCTYPE html>`), else none.
pub fn syntax_for(lang: &str, code: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let lang = lang.trim();
    let lower = lang.to_ascii_lowercase();
    if PLAIN.contains(&lower.as_str()) {
        return None;
    }
    let name = lang.rsplit(['/', '\\']).next().unwrap_or(lang);
    let by_name = || {
        let a = alias(&lower);
        ss.find_syntax_by_extension(a)
            .or_else(|| ss.find_syntax_by_token(a))
            .or_else(|| ss.find_syntax_by_extension(name))
            .or_else(|| {
                name.rsplit_once('.').and_then(|(_, ext)| ss.find_syntax_by_extension(&ext.to_ascii_lowercase()))
            })
    };
    let found = if lang.is_empty() { None } else { by_name() };
    found
        .filter(|s| s.name != "Plain Text")
        .or_else(|| ss.find_syntax_by_first_line(code.lines().next().unwrap_or("")))
        .or_else(|| sniff(code).and_then(|e| ss.find_syntax_by_extension(e)))
}

/// A guess from the shape of the text, for a fence with no label.
fn sniff(code: &str) -> Option<&'static str> {
    let t = code.trim_start();
    if (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<serde_json::Value>(code).is_ok() {
        Some("json")
    } else if t.starts_with("diff --git") || t.starts_with("--- a/") || t.starts_with("@@ ") {
        Some("diff")
    } else if t.starts_with("<html") || t.starts_with("<div") || t.starts_with("<!--") {
        Some("html")
    } else {
        None
    }
}

/// The language of a path, for `read` displays: the file name, which also tells
/// `Dockerfile`, `Makefile` or `.bashrc` apart.
pub fn lang_of_path(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_known_languages_and_leaves_others_plain() {
        let t = Theme::dark();
        let lines = highlight("fn main() {}\nlet x = 1;", "rust", &t);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].spans.len() > 1, "rust is split into styled spans");
        let lines = highlight("hello", "nosuchlang", &t);
        assert_eq!(lines[0].spans.len(), 1);
        let lines = highlight("x = 1", "py", &Theme::mono());
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lang_of_path("src/main.rs"), "main.rs");
        assert_eq!(lang_of_path("C:\\x\\Makefile"), "Makefile");
    }

    #[test]
    fn knows_the_languages_people_write_by_any_of_their_names() {
        let cases = [
            ("bash", "Bourne Again Shell (bash)"),
            ("sh", "Bourne Again Shell (bash)"),
            ("zsh", "Bourne Again Shell (bash)"),
            ("js", "JavaScript"),
            ("javascript", "JavaScript"),
            ("ts", "TypeScript"),
            ("tsx", "TypeScriptReact"),
            ("c", "C"),
            ("h", "C"),
            ("cpp", "C++"),
            ("c++", "C++"),
            ("C#", "C#"),
            ("python", "Python"),
            ("rust", "Rust"),
            ("go", "Go"),
            ("java", "Java"),
            ("kotlin", "Kotlin"),
            ("swift", "Swift"),
            ("zig", "Zig"),
            ("toml", "TOML"),
            ("yaml", "YAML"),
            ("json", "JSON"),
            ("sql", "SQL"),
            ("vue", "Vue Component"),
            ("terraform", "Terraform"),
            ("log", "log"),
            ("dockerfile", "Dockerfile"),
            ("lua", "Lua"),
            ("php", "PHP"),
            ("ruby", "Ruby"),
            ("haskell", "Haskell"),
            ("nix", "Nix"),
            ("src/app.tsx", "TypeScriptReact"),
            ("deploy/Dockerfile", "Dockerfile"),
            ("Makefile", "Makefile"),
            ("CMakeLists.txt", "CMake"),
        ];
        for (label, want) in cases {
            let got = syntax_for(label, "").map(|s| s.name.as_str());
            assert_eq!(got, Some(want), "{label}");
        }
        assert!(syntax_for("text", "#!/bin/bash").is_none(), "text stays text");
        assert_eq!(syntax_for("", "#!/usr/bin/env python3\nprint(1)").map(|s| s.name.as_str()), Some("Python"));
        assert_eq!(syntax_for("", "{\"a\": [1, 2]}").map(|s| s.name.as_str()), Some("JSON"));
        assert!(syntax_for("", "just words").is_none());
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(crate::render::markdown::text_of).collect()
    }

    #[test]
    fn interprets_markdown_json_and_tables_and_nothing_else() {
        let t = Theme::dark();
        let md = texts(&interpret("md", "# Title\n\n- one\n- two", 40, &t).unwrap());
        assert_eq!(md, ["# Title", "", "• one", "• two"]);
        let js = texts(&interpret("json", "{\"a\":[1,2],\"b\":\"x\"}", 40, &t).unwrap());
        assert_eq!(js.len(), 7, "indented: {js:?}");
        let csv = texts(&interpret("data.csv", "name,qty\n\"pear, green\",3\napple,12", 40, &t).unwrap());
        assert_eq!(csv[0], "name        │ qty");
        assert!(csv[1].starts_with("────"), "{csv:?}");
        assert_eq!(csv[2], "pear, green │   3");
        assert_eq!(csv[3], "apple       │  12");
        assert!(interpret("rs", "fn main() {}", 40, &t).is_none());
        assert!(interpret("json", "{broken", 40, &t).is_none());
        let (lines, done) = view("rs", "fn x() {}", 40, &t, true);
        assert!(!done && lines.len() == 1);
    }
}
