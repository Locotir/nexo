use std::collections::BTreeMap;

// Ranges share a label; occurrences are still counted separately by code point.
const MARKS: &[(u32, u32, &str)] = &[
    (0x200b, 0x200b, "zero-width space"),
    (0x200c, 0x200f, "invisible character"),
    (0x2060, 0x2064, "invisible character"),
    (0x00ad, 0x00ad, "soft hyphen"),
    (0xfeff, 0xfeff, "byte-order mark"),
    (0x202a, 0x202e, "bidirectional control"),
    (0x2066, 0x2069, "bidirectional control"),
    (0xe0000, 0xe007f, "tag character"),
    (0x2014, 0x2014, "em dash"),
    (0x2018, 0x2019, "curly quote"),
    (0x201c, 0x201d, "curly quote"),
    (0x2026, 0x2026, "ellipsis"),
    (0x00a0, 0x00a0, "no-break space"),
    (0x202f, 0x202f, "narrow no-break space"),
];

#[derive(Default)]
struct Counts {
    old: usize,
    new: usize,
    lines: Vec<usize>,
    more: bool,
}

pub fn added(old: &str, new: &str) -> Option<String> {
    added_with_locale(old, new, false)
}

pub fn added_for_path(old: &str, new: &str, path: &std::path::Path) -> Option<String> {
    let path = path.to_string_lossy().to_ascii_lowercase().replace('_', "-");
    let locale = ["pl-pl", "ro-ro"].iter().any(|locale| path.contains(locale));
    if locale {
        added_with_locale(old, new, true)
    } else {
        added(old, new)
    }
}

fn added_with_locale(old: &str, new: &str, locale: bool) -> Option<String> {
    let mut counts: BTreeMap<char, Counts> = BTreeMap::new();
    for (is_new, text) in [(false, old), (true, new)] {
        let mut line = 1;
        for (offset, c) in text.char_indices() {
            if c == '\n' {
                line += 1;
            }
            if c == '\u{feff}' && offset == 0 {
                continue;
            }
            if MARKS.iter().any(|&(a, b, _)| (a..=b).contains(&(c as u32))) {
                let count = counts.entry(c).or_default();
                if is_new {
                    count.new += 1;
                    if count.lines.last() != Some(&line) {
                        if count.lines.len() < 5 {
                            count.lines.push(line);
                        } else {
                            count.more = true;
                        }
                    }
                } else {
                    count.old += 1;
                }
            }
        }
    }
    let parts: Vec<_> = counts
        .into_iter()
        .filter(|(ch, c)| {
            c.new > c.old
                && !(matches!(*ch, '\u{2018}' | '\u{2019}' | '\u{201c}' | '\u{201d}' | '\u{2026}')
                    && (c.old > 0 || locale))
        })
        .map(|(ch, c)| {
            let name = MARKS.iter().find(|&&(a, b, _)| (a..=b).contains(&(ch as u32))).unwrap().2;
            let lines = c.lines.iter().map(usize::to_string).collect::<Vec<_>>().join(", ");
            let suffix = if c.more { ", ..." } else { "" };
            let plural = if c.new == 1 {
                ""
            } else if name == "em dash" {
                "es"
            } else {
                "s"
            };
            let was = if c.old == 0 { "had none before".to_string() } else { format!("had {} before", c.old) };
            format!(
                "{} {name}{plural} (U+{:04X}) at line{} {lines}{suffix} ({was})",
                c.new,
                ch as u32,
                if c.lines.len() == 1 && !c.more { "" } else { "s" }
            )
        })
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "new marks of machine-written text: {}. Use the file's own punctuation unless the person asked for these.",
            parts.join("; ")
        ))
    }
}

/// Keep unchanged characters, but remove newly inserted invisible controls.
pub fn strip_added(old: &str, new: &str) -> (String, String) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_chars(old, new);
    let mut cleaned = String::with_capacity(new.len());
    let mut removed = BTreeMap::<char, (usize, Vec<usize>)>::new();
    let mut line = 1usize;
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Delete {
            continue;
        }
        for c in change.value().chars() {
            let invisible = MARKS[..8].iter().any(|&(a, b, _)| (a..=b).contains(&(c as u32)));
            let keep_bom = c == '\u{feff}' && cleaned.is_empty();
            let existing_joiner = c == '\u{200d}' && old.contains(c);
            if change.tag() == ChangeTag::Insert && invisible && !keep_bom && !existing_joiner {
                let entry = removed.entry(c).or_default();
                entry.0 += 1;
                if entry.1.last() != Some(&line) && entry.1.len() < 5 {
                    entry.1.push(line);
                }
            } else {
                cleaned.push(c);
            }
            if c == '\n' {
                line += 1;
            }
        }
    }
    let details = removed
        .into_iter()
        .map(|(ch, (count, lines))| {
            let name = MARKS.iter().find(|&&(a, b, _)| (a..=b).contains(&(ch as u32))).unwrap().2;
            format!(
                "{count} {name} (U+{:04X}) at lines {}",
                ch as u32,
                lines.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>();
    let note = if details.is_empty() {
        String::new()
    } else {
        format!("\nremoved newly added invisible characters: {}", details.join("; "))
    };
    (cleaned, note)
}

#[cfg(test)]
mod tests {
    use super::added;
    #[test]
    fn reports_counts_and_lines() {
        let s = added("", "a—\nb\u{200b}\nc—").unwrap();
        assert!(s.contains("2 em dashes (U+2014) at lines 1, 3"), "{s}");
        assert!(s.contains("1 zero-width space (U+200B) at line 2"), "{s}");
    }
    #[test]
    fn locale_and_existing_typography_are_respected() {
        assert!(super::added_for_path("", "‘hi’…", std::path::Path::new("pl-PL/strings.txt")).is_none());
        assert!(super::added_for_path("", "‘hi’…", std::path::Path::new("ro_RO/strings.txt")).is_none());
        assert!(added("…", "……").is_none());
    }
    #[test]
    fn existing_count_and_bom_and_normal_text_are_not_reported() {
        assert!(added("—", "x—").is_none());
        assert!(added("", "\u{feff}hello").is_none());
        assert!(added("", "plain ASCII").is_none());
        assert!(added("", "«Español, sí»").is_none());
    }
}
