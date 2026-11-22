//! The answer to a `recall` call: a message the context left out, back as it was, from the whole
//! history the session keeps (the tool is `nexo_tools::recall`; the curator names messages `m#####`).

use nexo_core::message::{Message, Part};
use nexo_core::ToolOutput;
use serde_json::Value;

/// Characters one answer carries at most; a longer message comes back in parts (`from`, `to`, and `skip` within one long line).
pub const MAX_CHARS: usize = 60_000;

/// The place in the conversation (from 0) that a name like `m00042`, `m42` or `42` gives.
pub fn index_of(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches(['m', 'M', '#']);
    n.parse::<usize>().ok().filter(|&k| k > 0).map(|k| k - 1)
}

/// The text of a message as it was: its text, the calls it made and the outputs it carried.
pub fn message_text(m: &Message) -> String {
    let mut out: Vec<String> = Vec::new();
    for p in &m.content {
        match p {
            Part::Text { text } => out.push(text.clone()),
            Part::ToolCall { id, name, args } => out.push(format!("[call {name}, id={id}] {args}")),
            Part::ToolResult { call_id, name, content, is_error } => {
                let body: Vec<String> = content
                    .iter()
                    .filter_map(|c| match c {
                        Part::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                let what = name.as_deref().unwrap_or("tool");
                let err = if *is_error { " (error)" } else { "" };
                out.push(format!("[output of {what}{err}, call_id={call_id}]\n{}", body.join("\n")));
            }
            _ => {}
        }
    }
    out.join("\n")
}

/// What `recall` names a message by and the text it numbers. A message that only carries one output
/// numbers the output alone, so line k is line k of what the tool printed, as the curator's marks count it.
fn numbered(m: &Message) -> (String, String) {
    if let [Part::ToolResult { call_id, name, content, is_error }] = m.content.as_slice() {
        let body: Vec<&str> = content
            .iter()
            .filter_map(|c| match c {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let err = if *is_error { ", an error" } else { "" };
        return (
            format!(" (the output of {}{err}, call_id={call_id})", name.as_deref().unwrap_or("a tool")),
            body.join("\n"),
        );
    }
    (String::new(), message_text(m))
}

/// For a message that only makes calls: where their outputs are (the next message carries them).
fn outputs_at(history: &[Message], i: usize) -> Option<String> {
    let calls = history[i].content.iter().all(|p| matches!(p, Part::ToolCall { .. }));
    let next = history.get(i + 1)?;
    (calls && next.content.iter().any(|p| matches!(p, Part::ToolResult { .. })))
        .then(|| format!("\n(its output is m{:05}: recall that for what it printed)", i + 2))
}

/// The answer to a `recall` call over the whole `history`.
pub fn answer(history: &[Message], args: &Value) -> ToolOutput {
    answer_within(history, args, None)
}

/// [`answer`] held to the room one result may take of the model's window, when it is smaller than [`MAX_CHARS`]: a part
/// that is cut to it says so, besides where the rest is.
pub fn answer_within(history: &[Message], args: &Value, room: Option<nexo_core::ResultRoom>) -> ToolOutput {
    let max = room.map_or(MAX_CHARS, |r| r.cap(MAX_CHARS));
    let window =
        room.filter(|_| max < MAX_CHARS).map(|r| format!("; cut to {max} characters for a {}-token window", r.window));
    let window = window.unwrap_or_default();
    let Some(name) = args.get("message").and_then(Value::as_str) else {
        return ToolOutput::error("`message` is required: the name the message was left out under, like m00042");
    };
    let Some(i) = index_of(name) else {
        return ToolOutput::error(format!("`{name}` is not a message name: use the form m00042"));
    };
    let Some(m) = history.get(i) else {
        return ToolOutput::error(format!("there is no message {name}: the conversation has {}", history.len()));
    };
    let (what, text) = numbered(m);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len().max(1);
    let line = |k: &str| args.get(k).and_then(Value::as_u64).map(|v| v as usize);
    let from = line("from").unwrap_or(1).clamp(1, total);
    let to = line("to").unwrap_or(total).clamp(from, total);
    let skip = line("skip").unwrap_or(0);
    let first = lines.get(from - 1).copied().unwrap_or("");
    // a line longer than one answer holds (a minified file, a dump) comes back in parts of its own
    // characters: `skip` says how many of the line's characters were already brought back
    if skip > 0 || first.len() + 1 > max {
        let len = first.chars().count();
        let skip = skip.min(len);
        let mut end = skip;
        let mut bytes = 0;
        for c in first.chars().skip(skip) {
            bytes += c.len_utf8();
            if bytes > max {
                break;
            }
            end += 1;
        }
        let piece: String = first.chars().skip(skip).take(end - skip).collect();
        let more = if end < len {
            format!(" (the rest of this line: recall with from={from}, skip={end}{window})")
        } else if from < to {
            format!(" (the next line: recall with from={})", from + 1)
        } else {
            String::new()
        };
        let head = format!("m{:05}{what}, line {from} of {total}, characters {skip}-{end} of {len}{more}:", i + 1);
        let tail = if end >= len && from >= total { outputs_at(history, i).unwrap_or_default() } else { String::new() };
        return ToolOutput::ok(format!("{head}\n{piece}{tail}"));
    }
    let mut body = String::new();
    let mut last = from - 1;
    for l in lines.iter().take(to).skip(from - 1) {
        if body.len() + l.len() + 1 > max && last >= from {
            break;
        }
        body.push_str(l);
        body.push('\n');
        last += 1;
    }
    let head = if from == 1 && last == total {
        format!("m{:05}{what}, as it was:", i + 1)
    } else if last < to {
        format!(
            "m{:05}{what}, lines {from}-{last} of {total} (the rest: recall with from={}{window}):",
            i + 1,
            last + 1
        )
    } else {
        format!("m{:05}{what}, lines {from}-{last} of {total}:", i + 1)
    };
    let tail = outputs_at(history, i).unwrap_or_default();
    ToolOutput::ok(format!("{head}\n{}{tail}", body.trim_end_matches('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn history() -> Vec<Message> {
        let out: String = (1..=5).map(|k| format!("row {k}\n")).collect();
        vec![
            Message::user("Count the rows."),
            Message::new(
                nexo_core::message::Role::Assistant,
                vec![Part::ToolCall { id: "c1".into(), name: "bash".into(), args: json!({"command": "cat t"}) }],
            ),
            Message::tool_results(vec![Part::ToolResult {
                call_id: "c1".into(),
                name: Some("bash".into()),
                content: vec![Part::text(out)],
                is_error: false,
            }]),
        ]
    }

    #[test]
    fn a_message_comes_back_exactly() {
        let o = answer(&history(), &json!({"message": "m00003"}));
        assert!(!o.is_error, "{}", o.content);
        assert!(o.content.starts_with("m00003 (the output of bash, call_id=c1), as it was:\nrow 1\n"), "{}", o.content);
        assert!(o.content.ends_with("row 5"), "{}", o.content);
        assert_eq!(index_of("m3"), Some(2));
        assert_eq!(index_of("3"), Some(2));
        assert_eq!(index_of("m0"), None);
    }

    #[test]
    fn part_of_a_message_and_bad_names() {
        let o = answer(&history(), &json!({"message": "m00003", "from": 3, "to": 4}));
        assert_eq!(o.content, "m00003 (the output of bash, call_id=c1), lines 3-4 of 5:\nrow 3\nrow 4");
        let o = answer(&history(), &json!({"message": "m00002"}));
        assert_eq!(o.content, "m00002, as it was:\n[call bash, id=c1] {\"command\":\"cat t\"}\n(its output is m00003: recall that for what it printed)");
        assert!(answer(&history(), &json!({"message": "m00009"})).is_error);
        assert!(answer(&history(), &json!({"message": "x"})).is_error);
        assert!(answer(&history(), &json!({})).is_error);
    }

    #[test]
    fn parallel_calls_and_results_keep_their_ids() {
        let history = vec![
            Message::new(
                nexo_core::message::Role::Assistant,
                vec![
                    Part::ToolCall { id: "a".into(), name: "read".into(), args: json!({"path":"old"}) },
                    Part::ToolCall { id: "b".into(), name: "read".into(), args: json!({"path":"new"}) },
                ],
            ),
            Message::tool_results(vec![
                Part::ToolResult {
                    call_id: "b".into(),
                    name: Some("read".into()),
                    content: vec![Part::text("30 days")],
                    is_error: false,
                },
                Part::ToolResult {
                    call_id: "a".into(),
                    name: Some("read".into()),
                    content: vec![Part::text("60 days")],
                    is_error: false,
                },
            ]),
        ];
        let calls = answer(&history, &json!({"message":"m1"})).content;
        let results = answer(&history, &json!({"message":"m2"})).content;
        assert!(calls.contains("[call read, id=a] {\"path\":\"old\"}"));
        assert!(calls.contains("[call read, id=b] {\"path\":\"new\"}"));
        assert!(results.contains("[output of read, call_id=b]\n30 days"));
        assert!(results.contains("[output of read, call_id=a]\n60 days"));
    }

    #[test]
    fn one_huge_line_comes_back_in_parts_the_header_says_how_to_follow() {
        let line = "é".repeat(200_000);
        let h = vec![Message::user(line.clone()), Message::assistant("ok")];
        let o = answer(&h, &json!({"message": "m00001"}));
        assert!(!o.is_error);
        let (head, body) = o.content.split_once(":\n").unwrap();
        assert!(body.len() <= MAX_CHARS && body.chars().all(|c| c == 'é'), "{}", body.len());
        let end = body.chars().count();
        assert_eq!(
            head,
            format!(
                "m00001, line 1 of 1, characters 0-{end} of 200000 (the rest of this line: recall with from=1, skip={end})"
            )
        );
        // following the header until the end gives the whole line back
        let mut got = body.to_string();
        let mut skip = end;
        while got.chars().count() < 200_000 {
            let o = answer(&h, &json!({"message": "m00001", "from": 1, "skip": skip}));
            let (head, body) = o.content.split_once(":\n").unwrap();
            assert!(head.contains(&format!("characters {skip}-")), "{head}");
            skip += body.chars().count();
            got.push_str(body);
        }
        assert_eq!(got, line);
    }
}
