//! The facts nexo writes in a request about the requests before it: that the person stopped the one above, when
//! and while what ran (`[16:13:08 the person stopped the request above (Esc) while ...]`), and, when messages typed
//! one after another go to the model as one, the time each was typed (`[16:13:05, typed while you worked on the
//! request above]`). The model reads which line came first and what happened in between instead of one block whose
//! older half reads as part of the newest request. The curator reads the same lines (`nexo_curator::msg`).

use super::person_words;

/// What follows the time in the line that says the person stopped the request before this one.
pub const STOP_WORDS: &str = "the person stopped the request above";
/// What follows the time in the line that says something other than the person ended the request before this one
/// (the client that drives nexo, the run's time limit): `the request above was stopped by the client's cancel`.
pub const STOPPED_BY: &str = "the request above was stopped by";
/// The line before the parts of messages nexo joined into one. Without it a message whose own lines look like a
/// header (a pasted chat log with `[16:13:05]` lines) would read as messages the person typed one by one.
pub const JOINED_MARK: &str = "[messages typed one after another]";
/// What a stop line says, before what ran, of the request it names: `; the request was "..."`.
pub const REQUEST_WAS: &str = "; the request was \"";
/// The most characters of a request a stop line quotes.
const QUOTED: usize = 50;
/// The most characters of one call's summary in a stop line.
pub const CALL_SHOWN: usize = 80;

/// Who or what ended a request, and how the person did it when they did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stopper {
    /// Esc in the terminal interface.
    Esc,
    /// Ctrl+C, in the terminal interface or a headless run.
    CtrlC,
    /// The client driving nexo (ACP) cancelled the request.
    Client,
    /// The run's time limit (the benchmark harness), not the person.
    TimeLimit,
}

impl Stopper {
    /// How a stop line says it, from the words that follow its time.
    pub fn words(self) -> String {
        match self {
            Stopper::Esc => format!("{STOP_WORDS} (Esc)"),
            Stopper::CtrlC => format!("{STOP_WORDS} (Ctrl+C)"),
            Stopper::Client => format!("{STOPPED_BY} the client's cancel"),
            Stopper::TimeLimit => format!("{STOPPED_BY} the run's time limit"),
        }
    }
}

/// `s` on one line: its whitespace, newlines included, as single spaces, cut to `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// What the person asked in the message `text` (as the model got it), as a stop line quotes it: the newest part of
/// messages joined into one, on one line and cut short, with `"` as `'` so the quote has one end.
pub fn request_quote(text: &str) -> String {
    let words = person_words(text);
    let words = stop_line_of(words).map_or(words, |l| words[l.len()..].trim_start());
    let newest = joined_parts(words).and_then(|p| p.last().copied()).unwrap_or(words);
    one_line(&newest.replace('"', "'"), QUOTED)
}
/// What a part's header says after its time when the part was typed while the model worked.
pub const TYPED_DURING: &str = "typed while you worked on the request above";
/// What it adds when the person then stopped that request.
pub const BEFORE_STOP: &str = "before the person stopped it";

/// What a person-message clock line says after the gap.
const AFTER_PREVIOUS: &str = " after your previous message";

/// How long a gap between two of the person's messages reads: `under a minute`, `7 min`, `3 h 5 min`, `2 days 4 h`.
pub fn gap_words(gap: std::time::Duration) -> String {
    let mins = gap.as_secs() / 60;
    match mins {
        0 => "under a minute".to_string(),
        1..=59 => format!("{mins} min"),
        60..=1439 => match mins % 60 {
            0 => format!("{} h", mins / 60),
            m => format!("{} h {m} min", mins / 60),
        },
        _ => {
            let (d, h) = (mins / 1440, (mins / 60) % 24);
            let unit = if d == 1 { "day" } else { "days" };
            if h == 0 {
                format!("{d} {unit}")
            } else {
                format!("{d} {unit} {h} h")
            }
        }
    }
}

/// The line before the person's words that says when they wrote them (`at`, `HH:MM:SS`) and, when the person wrote
/// before, how long after their previous message: `[16:13:05, 7 min after your previous message]`.
pub fn person_clock(at: &str, gap: Option<std::time::Duration>) -> String {
    match gap {
        Some(g) => format!("[{at}, {}{AFTER_PREVIOUS}]", gap_words(g)),
        None => format!("[{at}]"),
    }
}

/// Whether `line` is the clock line [`person_clock`] writes before a person's message.
pub fn is_person_clock(line: &str) -> bool {
    timed(line.trim_end())
        .is_some_and(|(_, rest)| rest.is_empty() || (rest.starts_with(", ") && rest.ends_with(AFTER_PREVIOUS)))
}

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
/// running then with how the request was left (`while you waited for your reply; it got no answer`). Always one
/// line: what a call's summary holds of newlines is spaces here.
pub fn stop_line(at: &str, how: &str, what: &str) -> String {
    format!("[{at} {STOP_WORDS} {} {}]", one_line(how, 200), one_line(what, 800))
}

/// The same line for a request ended by `by`: the person with Esc or Ctrl+C, the client, the run's time limit.
pub fn stop_line_by(at: &str, by: Stopper, what: &str) -> String {
    format!("[{at} {} {}]", by.words(), one_line(what, 800))
}

/// The stop line `text` starts with, if it starts with one.
pub fn stop_line_of(text: &str) -> Option<&str> {
    let first = text.split('\n').next()?;
    let (_, rest) = timed(first)?;
    let rest = rest.strip_prefix(' ')?;
    (rest.starts_with(STOP_WORDS) || rest.starts_with(STOPPED_BY)).then_some(first)
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
/// None when the words are one message (they do not start with [`JOINED_MARK`], the line nexo writes before the
/// first header: a header alone could be the person's own text).
pub fn joined_parts(words: &str) -> Option<Vec<&str>> {
    let mut lines = words.split_inclusive('\n');
    if lines.next()?.trim_end_matches(['\n', '\r']) != JOINED_MARK {
        return None;
    }
    let first = lines.next()?;
    if !is_part_header(first.trim_end_matches(['\n', '\r'])) {
        return None;
    }
    let mut parts = Vec::new();
    let top = words.find('\n').map_or(0, |i| i + 1);
    let mut start = top + first.len();
    let mut at = start;
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
    use crate::agent::PERSON_MARK;

    #[test]
    fn a_gap_reads_in_the_unit_that_fits_it() {
        let s = std::time::Duration::from_secs;
        assert_eq!(gap_words(s(5)), "under a minute");
        assert_eq!(gap_words(s(7 * 60 + 30)), "7 min");
        assert_eq!(gap_words(s(3 * 3600 + 5 * 60)), "3 h 5 min");
        assert_eq!(gap_words(s(2 * 86400 + 4 * 3600)), "2 days 4 h");
        assert_eq!(gap_words(s(86400)), "1 day");
        assert!(is_person_clock("[10:00:00]") && is_person_clock(&person_clock("10:00:00", Some(s(300)))));
        assert!(!is_person_clock("[10:00:00 the person stopped it]"));
        assert_eq!(person_clock("10:00:00", None), "[10:00:00]");
    }

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
            "{JOINED_MARK}\n{}\ny documenta cada función\n\n{}\nDetente, cambiamos de tema",
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
            "{JOINED_MARK}\n{}\ny documenta cada función\n\n{}\nDetente, cambiamos de tema",
            part_header("16:13:05", true, true),
            part_header("16:13:10", false, false)
        );
        assert_eq!(as_typed(&words), "y documenta cada función\n\nDetente, cambiamos de tema");
        assert_eq!(as_typed("[12:00:00 is when it broke]\nfix it"), "[12:00:00 is when it broke]\nfix it");
    }

    #[test]
    fn a_pasted_log_with_time_lines_stays_the_persons_text_and_only_nexos_joined_messages_split() {
        let pasted = "[16:13:05]\nalice: it broke\n\n[16:13:10]\nbob: since when";
        assert_eq!(joined_parts(pasted), None);
        assert_eq!(as_typed(pasted), pasted);
        let joined = format!("{JOINED_MARK}\n[16:13:05]\n{pasted}\n\n[16:13:20]\nfix it");
        let parts = joined_parts(&joined).unwrap();
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(parts.last(), Some(&"fix it"));
    }

    #[test]
    fn a_stop_line_is_one_line_whatever_the_call_summaries_hold() {
        let line =
            stop_line("16:13:08", "(Esc)", "while ask: which file?\n\nthe long one\r\nran; the request ended there");
        assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        assert_eq!(stop_line_of(&format!("{line}\n\nnext")), Some(line.as_str()));
        assert!(line.contains("ask: which file? the long one ran"), "{line}");
    }

    #[test]
    fn a_stop_line_says_who_ended_the_request_as_it_really_was() {
        let what = "while you waited for your reply; it got no answer";
        let line = |by| stop_line_by("16:13:08", by, what);
        assert!(line(Stopper::Esc).starts_with("[16:13:08 the person stopped the request above (Esc) while"));
        assert!(line(Stopper::CtrlC).contains("the person stopped the request above (Ctrl+C)"));
        for (by, who) in [(Stopper::Client, "the client's cancel"), (Stopper::TimeLimit, "the run's time limit")] {
            let l = line(by);
            assert!(l.contains(who) && !l.contains("the person"), "{l}");
            assert_eq!(stop_line_of(&format!("{l}\n\nnext")), Some(l.as_str()));
        }
    }

    #[test]
    fn a_request_is_quoted_by_its_newest_part_on_one_line_without_double_quotes() {
        let joined = format!("{JOINED_MARK}\n[16:13:05]\nold one\n\n[16:13:10]\nsay \"hi\"\nplease");
        assert_eq!(request_quote(&format!("note\n\n{PERSON_MARK}\n{joined}")), "say 'hi' please");
        assert!(request_quote(&"x ".repeat(100)).ends_with('…'));
    }
}
