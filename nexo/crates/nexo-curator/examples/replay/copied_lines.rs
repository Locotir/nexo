use super::*;

/// Every string inside `v`, however deep.
fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// Lab measure (L3): of the lines of 12 characters or more that the reply writes in a call and a result of the
/// history holds (an edit's old text, a patch), those that no message of the view holds: (missing, all).
/// A call that repeats a line of a result it can no longer see cannot be written right, so this is the
/// check for a view rule before it is turned on.
pub(super) fn copied_lines(reply: &Value, history: &[Value], view: &[Value]) -> (i64, i64) {
    let mut calls: Vec<&str> = Vec::new();
    if let Some(Value::Array(blocks)) = reply.get("content") {
        for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
            strings(&b["input"], &mut calls);
        }
    }
    let wrote: HashSet<&str> =
        calls.iter().flat_map(|s| s.split('\n')).map(str::trim).filter(|l| l.len() >= 12).collect();
    if wrote.is_empty() {
        return (0, 0);
    }
    let mut held: HashSet<&str> = HashSet::new();
    for m in history {
        if let Some(Value::Array(blocks)) = m.get("content") {
            for b in blocks.iter().filter(|b| b["type"] == "tool_result") {
                let mut ss = Vec::new();
                strings(&b["content"], &mut ss);
                for l in ss.iter().flat_map(|s| s.split('\n')).map(str::trim) {
                    if wrote.contains(l) {
                        held.insert(l);
                    }
                }
            }
        }
    }
    if held.is_empty() {
        return (0, 0);
    }
    let mut ss = Vec::new();
    for m in view {
        strings(m, &mut ss);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for l in ss.iter().flat_map(|s| s.split('\n')).map(str::trim) {
        if held.contains(l) {
            seen.insert(l);
        }
    }
    ((held.len() - seen.len()) as i64, held.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_copied_from_a_result_the_view_lacks_is_lost() {
        let file = "alpha line one\nbeta line two\ngamma line three\ndelta line four\nepsilon line five";
        let history = [
            json!({"role": "user", "content": "fix the file"}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "read", "input": {"p": "f"}}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": file}]}),
        ];
        let reply = json!({"role": "assistant", "content": [{"type": "tool_use", "id": "b", "name": "edit",
            "input": {"old": "alpha line one\nbeta line two\ngamma line three", "new": "x"}}]});
        let view = [
            history[0].clone(),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "beta line two"}]}),
        ];
        assert_eq!(copied_lines(&reply, &history, &view), (2, 3));
        assert_eq!(copied_lines(&reply, &history, &history), (0, 3));
    }
}
