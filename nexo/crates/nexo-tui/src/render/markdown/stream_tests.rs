//! Markdown samples and a small random generator shared by the streaming tests and the timing test.

/// A reply of about `lines` lines that mixes prose, nested and numbered lists, fenced code in several
/// languages, tables, headings, quotes and rules, the way a long answer reads.
pub(crate) fn long_reply(lines: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut section = 0usize;
    while out.lines().count() < lines {
        n += 1;
        match section % 8 {
            0 => out.push_str(&format!("## Step {n}: reading the module\n\n")),
            1 => out.push_str(&format!(
                "The function `parse_{n}` reads the **input** once and keeps a [cursor](https://example.com/{n}) \
                 into it, so the cost is linear in the size of the file and the memory stays flat while it works, \
                 which matters when a build log grows to many megabytes.\n\n"
            )),
            2 => out.push_str(&format!(
                "- first point of {n}\n- second point with `code` inside\n  - nested one\n  - nested two\n    - deeper still\n- third point\n\n"
            )),
            3 => out.push_str("1. open the file\n2. read the header\n3. check the version\n4. decode the body\n5. close it\n\n"),
            4 => {
                let (lang, body) = match n % 5 {
                    0 => ("rust", "fn main() {\n    let v: Vec<u32> = (0..10).collect();\n\n    for x in &v {\n        println!(\"{x}\");\n    }\n}"),
                    1 => ("python", "def total(xs):\n    s = 0\n\n    for x in xs:\n        s += x\n    return s"),
                    2 => ("json", "{\n  \"name\": \"nexo\",\n\n  \"items\": [1, 2, 3]\n}"),
                    3 => ("bash", "set -e\n\nfor f in *.rs; do\n  wc -l \"$f\"\ndone"),
                    _ => ("typescript", "export function add(a: number, b: number): number {\n  return a + b;\n}"),
                };
                out.push_str(&format!("```{lang}\n{body}\n```\n\n"));
            }
            5 => out.push_str(&format!(
                "| name | size | note |\n|------|-----:|------|\n| a{n} | 10 | first |\n| b{n} | 2000 | second row |\n| c{n} | 3 | third |\n\n"
            )),
            6 => out.push_str("> a quoted remark that goes over a few words\n> and carries on in the next line\n\n"),
            _ => out.push_str("Closing words for this part, with *emphasis* and a ~~struck~~ word.\n\n---\n\n"),
        }
        section += 1;
    }
    out
}

/// Small documents that stress the places where a split could go wrong.
pub(crate) fn samples() -> Vec<String> {
    vec![
        long_reply(60),
        "# Title\n\nSome text\nthat goes on\n\nAnother paragraph\n===\n\nFinal one\n".to_string(),
        "1. one\n2. two\n\nnot a list item\n\n3. three\n4. four\n\n\n5. five\n\ntail\n".to_string(),
        "- a\n- b\n\n  continued in b\n\n- c\n\nAfter the list.\n\nMore.\n".to_string(),
        "```rust\nlet a = 1;\n\nlet b = 2;\n```\n\nafter\n\n~~~\nplain\n\nfence\n~~~\n\nend\n".to_string(),
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n| wide column here | b |\n|---|---|\n| 1 | 2 |\n\ntext\n".to_string(),
        "> quote one\n> still\n\n> quote two\n\nplain\n\n    indented code\n\n    more code\n\nback\n".to_string(),
        "Text with a [ref link][r] and more.\n\nAnother paragraph.\n\n[r]: https://example.com\n\nEnd.\n".to_string(),
        "<div>\nhtml block\n</div>\n\ntext after\n\nmore text\n".to_string(),
        "Intro\n\n- [ ] todo\n- [x] done\n\n9. nine\n10. ten\n\n```json\n{\"a\": 1}\n```\n\nBye\n".to_string(),
        "Setext\n---\n\nBefore a rule\n\n***\n\nAfter\n\n10. ten\n11. eleven\n\n\nlast\n".to_string(),
        "tab\there\n\nsecond\tparagraph\n\n```\ncode\twith tab\n```\n\nend\n".to_string(),
    ]
}

/// A tiny xorshift generator, so the splits are many but repeatable.
pub(crate) struct Rng(pub u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Pieces that documents are built from at random: every kind of block, and the ones that look like the
/// start of another.
const PIECES: &[&str] = &[
    "Plain words that make a paragraph.",
    "Two lines\nin one paragraph",
    "Hard break  \nafter spaces",
    "# A heading",
    "Setext heading\n===========",
    "- one\n- two\n  - nested\n- three",
    "* star item\n* another",
    "1. first\n2. second\n3. third",
    "9. nine\n10. ten\n11. eleven",
    "1) paren\n2) list",
    "- [ ] todo\n- [x] done",
    "- loose item\n\n- another loose item\n\n  with a second paragraph",
    "> a quote\n> goes on\n\nlazy",
    "> quote\nlazy continuation",
    "```rust\nfn a() {}\n\nfn b() {}\n```",
    "```\nno language\n\nwith a blank\n```",
    "~~~python\nx = 1\n\ny = 2\n~~~",
    "````md\n```\ninner\n\nfence\n```\n````",
    "```json\n{\"a\": [1, 2]}\n```",
    "    indented code\n\n    more",
    "| a | b |\n|---|---|\n| 1 | 2 |",
    "| name | n |\n|:-----|--:|\n| x | 10 |\n| yy | 200 |",
    "---",
    "***",
    "text\n---",
    "**Bold start** then words",
    "*emphasis* and `code` and ~~gone~~",
    "A [link](https://example.com/a) here",
    "![alt](https://example.com/i.png)",
    "- item\n\n```sh\necho hi\n\necho there\n```",
    "1. step\n\n   ```sh\n   run\n\n   again\n   ```\n\n2. next",
    "a | b\n--|--\n1 | 2",
    "<span>inline html</span> start",
    "line with trailing words:\n\n- a\n- b",
];

/// A document of random pieces joined by one to three line breaks.
pub(crate) fn random_doc(rng: &mut Rng) -> String {
    let n = 4 + (rng.next() % 9) as usize;
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str(["\n", "\n\n", "\n\n", "\n\n\n"][(rng.next() % 4) as usize]);
        }
        out.push_str(PIECES[(rng.next() % PIECES.len() as u64) as usize]);
    }
    out.push('\n');
    out
}
