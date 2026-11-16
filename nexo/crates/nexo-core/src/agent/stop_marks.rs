//! The facts nexo writes in a request about the requests before it: that the person stopped the one above, when
//! and while what ran (`[16:13:08 the person stopped the request above (Esc) while ...]`), and, when messages typed
//! one after another go to the model as one, the time each was typed (`[16:13:05, typed while you worked on the
//! request above]`). The model reads which line came first and what happened in between instead of one block whose
//! older half reads as part of the newest request. The curator reads the same lines (`nexo_curator::msg`).

/// What follows the time in the line that says the person stopped the request before this one.
pub const STOP_WORDS: &str = "the person stopped the request above";
/// What a part's header says after its time when the part was typed while the model worked.
pub const TYPED_DURING: &str = "typed while you worked on the request above";
/// What it adds when the person then stopped that request.
pub const BEFORE_STOP: &str = "before the person stopped it";

/// Whether `s` is a time as these lines write it: `HH:MM:SS`.
fn is_hms(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 8 && b[2] == b':' && b[5] == b':' && [0, 1, 3, 4, 6, 7].iter().all(|&i| b[i].is_ascii_digit())
}

/// The time and the rest of a line `[HH:MM:SS...]`.
fn timed(line: &str) -> Option<(&str, &str)> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (t, rest) = inner.split_at_checked(8)?;
    is_hms(t).then_some((t, rest))
}

/// The line that says the person stopped the request before: at `at` (`HH:MM:SS`), `how` (`(Esc)`), and what was
/// running then with how the request was left (`while you waited for your reply; it got no answer`).
pub fn stop_line(at: &str, how: &str, what: &str) -> String {
    format!("[{at} {STOP_WORDS} {how} {what}]")
}

/// The stop line `text` starts with, if it starts with one.
pub fn stop_line_of(text: &str) -> Option<&str> {
    let first = text.split('\n').next()?;
    let (_, rest) = timed(first)?;
    rest.strip_prefix(' ')?.starts_with(STOP_WORDS).then_some(first)
}

/// The header of one part of messages joined into one: when it was typed, and whether the model was working then
/// (`during`) on a request the person went on to stop (`stopped`).
pub fn part_header(at: &str, during: bool, stopped: bool) -> String {
    match (during, stopped) {
        (true, true) => format!("[{at}, {TYPED_DURING}, {BEFORE_STOP}]"),
        (true, false) => format!("[{at}, {TYPED_DURING}]"),
        _ => format!("[{at}]"),
    }
}

/// Whether `line` is the header of a part of joined messages ([`part_header`]).
pub fn is_part_header(line: &str) -> bool {
    timed(line).is_some_and(|(_, rest)| rest.is_empty() || rest.starts_with(", typed while"))
}

/// The parts of the person's words when they are messages joined into one, oldest first, each without its header;
/// None when the words are one message (they do not start with a header).
pub fn joined_parts(words: &str) -> Option<Vec<&str>> {
    let mut lines = words.split_inclusive('\n');
    let first = lines.next()?;
    if !is_part_header(first.trim_end_matches(['\n', '\r'])) {
        return None;
    }
    let mut parts = Vec::new();
    let mut start = first.len();
    let mut at = first.len();
    for line in lines {
        if is_part_header(line.trim_end_matches(['\n', '\r'])) {
            parts.push(words[start..at].trim_matches('\n'));
            start = at + line.len();
        }
        at += line.len();
    }
    parts.push(words[start..].trim_matches('\n'));
    Some(parts)
}

/// The person's words as they typed them: of messages joined into one, the parts one after another without the
/// headers nexo wrote before each; one message as it is. What /undo and a rewind put back in the editor, so a request
/// sent again carries no time of the first sending as if the person had typed it.
pub fn as_typed(words: &str) -> std::borrow::Cow<'_, str> {
    match joined_parts(words) {
        Some(parts) => parts.join("\n\n").into(),
        None => words.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_line_is_found_at_the_start_of_a_message_and_nowhere_else() {
        let line = stop_line("16:13:08", "(Esc)", "while you waited for your reply; it got no answer");
        assert_eq!(
            line,
            "[16:13:08 the person stopped the request above (Esc) while you waited for your reply; it got no answer]"
        );
        assert_eq!(stop_line_of(&format!("{line}\n\nnext")), Some(line.as_str()));
        assert_eq!(stop_line_of(&format!("words\n{line}")), None);
        assert_eq!(stop_line_of("[16:13 the person stopped the request above]"), None);
    }

    #[test]
    fn joined_words_split_into_their_parts_and_one_message_does_not() {
        let words = format!(
            "{}\ny documenta cada función\n\n{}\nDetente, cambiamos de tema",
            part_header("16:13:05", true, true),
            part_header("16:13:10", false, false)
        );
        assert_eq!(joined_parts(&words), Some(vec!["y documenta cada función", "Detente, cambiamos de tema"]));
        assert_eq!(joined_parts("Detente, cambiamos de tema"), None);
        assert_eq!(joined_parts("[12:00:00 is when it broke]\nfix it"), None);
    }

    #[test]
    fn joined_words_read_back_as_typed_have_no_headers_and_one_message_is_unchanged() {
        let words = format!(
            "{}\ny documenta cada función\n\n{}\nDetente, cambiamos de tema",
            part_header("16:13:05", true, true),
            part_header("16:13:10", false, false)
        );
        assert_eq!(as_typed(&words), "y documenta cada función\n\nDetente, cambiamos de tema");
        assert_eq!(as_typed("[12:00:00 is when it broke]\nfix it"), "[12:00:00 is when it broke]\nfix it");
    }
}
