//! Reading the shell text out of an AppleScript `do shell script` call.

/// Pushes the shell text of every AppleScript `do shell script "..."` in `script` (its quotes and backslashes
/// undone) onto `out`. Returns true when a shell call is there that this cannot read in full: a command built
/// by `&` or a variable, a call in JavaScript (`doShellScript`), an open string. AppleScript ignores case and
/// the spacing between the three words, so those are folded before looking, and every call the plain text
/// holds is counted against the ones read, so a spelling this does not parse counts as unreadable.
pub fn osascript_scripts(script: &str, out: &mut Vec<String>) -> bool {
    let packed: String = script.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let calls = packed.matches("doshellscript").count();
    if calls == 0 {
        return false;
    }
    let spaced = script.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = spaced.to_ascii_lowercase();
    let (mut read, mut unreadable) = (0, false);
    let mut from = 0;
    while let Some(at) = lower[from..].find("do shell script") {
        let start = from + at;
        let end = start + "do shell script".len();
        from = end;
        let word_edge = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !word_edge(lower[..start].chars().next_back()) || !word_edge(lower[end..].chars().next()) {
            continue;
        }
        read += 1;
        let mut rest = spaced[end..].trim_start();
        while let Some(r) = rest.strip_prefix('(') {
            rest = r.trim_start();
        }
        let Some(body) = rest.strip_prefix('"') else {
            unreadable = true;
            continue;
        };
        let (mut text, mut chars, mut closed) = (String::new(), body.char_indices(), None);
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    closed = Some(i + 1);
                    break;
                }
                '\\' => match chars.next() {
                    Some((_, 'n')) => text.push('\n'),
                    Some((_, 'r')) => text.push('\r'),
                    Some((_, 't')) => text.push('\t'),
                    Some((_, c)) => text.push(c),
                    None => break,
                },
                c => text.push(c),
            }
        }
        let Some(closed) = closed else {
            unreadable = true;
            continue;
        };
        // only the closing parenthesis and AppleScript's own `with administrator privileges` may follow: a `&`
        // or `+` goes on building the command
        let tail = body[closed..].trim_start_matches([')', ' ']).to_ascii_lowercase();
        unreadable |= !(tail.is_empty() || tail.starts_with("with ") || tail.starts_with("without "));
        out.push(text);
    }
    unreadable || read != calls
}
