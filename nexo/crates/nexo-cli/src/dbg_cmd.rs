//! `nexo dbg`: run a program under a debugger, or attach to one, and drive it one bounded call at a time.
//! The program lives in a small server (see the `nexo-dbg` crate) between two calls.

use std::path::PathBuf;

use clap::Args;
use nexo_bin::{Output, DEFAULT_LINES, MIN_LINES};
use nexo_dbg::{Error, Kind};
use serde_json::json;

#[derive(Args, Debug)]
pub struct DbgArgs {
    /// `run <exe> [args]`, `attach <pid>`, `sessions`, `end`, or a command for a live session (`help` lists them).
    /// Also: `-s <id>` picks a session when more than one is live; `--lines N`, `--from X`, `--json` as in `nexo bin`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., value_name = "COMMAND")]
    pub words: Vec<String>,
}

struct Opts {
    session: Option<String>,
    lines: usize,
    from: Option<u64>,
    json: bool,
    words: Vec<String>,
}

fn options(words: &[String]) -> Result<Opts, Error> {
    let mut o = Opts { session: None, lines: DEFAULT_LINES, from: None, json: false, words: Vec::new() };
    let mut it = words.iter();
    while let Some(w) = it.next() {
        // the first words are ours; `--lines` and the like inside a program's own arguments are left alone
        let ours = o.words.first().is_none_or(|c| c != "run");
        match w.as_str() {
            "-s" | "--session" if ours => {
                o.session = Some(it.next().ok_or_else(|| Error::Usage("-s needs a session id".into()))?.clone())
            }
            "--lines" if ours => {
                o.lines = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| Error::Usage("--lines needs a number".into()))?
            }
            "--from" if ours => {
                let v = it.next().ok_or_else(|| Error::Usage("--from needs an address".into()))?;
                o.from = Some(nexo_bin::parse_addr(v)?);
            }
            "--json" if ours => o.json = true,
            _ => o.words.push(w.clone()),
        }
    }
    Ok(o)
}

fn serve_args(w: &[String]) -> Result<nexo_dbg::Start, Error> {
    let (mut id, mut dir, mut owner, mut run, mut attach) = (None, None, None, None, None);
    let mut args = Vec::new();
    let mut it = w.iter();
    while let Some(k) = it.next() {
        let v = it.next().ok_or_else(|| Error::Usage(format!("{k} needs a value")))?;
        match k.as_str() {
            "--id" => id = Some(v.clone()),
            "--dir" => dir = Some(PathBuf::from(v)),
            "--owner" => owner = v.parse().ok(),
            "--run" => run = Some(PathBuf::from(v)),
            "--arg" => args.push(v.clone()),
            "--attach" => attach = v.parse().ok(),
            other => return Err(Error::Usage(format!("unknown server option {other}"))),
        }
    }
    let kind = match (run, attach) {
        (Some(exe), _) => Kind::Run { exe, args },
        (None, Some(pid)) => Kind::Attach(pid),
        _ => return Err(Error::Usage("the server needs --run or --attach".into())),
    };
    Ok(nexo_dbg::Start {
        id: id.ok_or_else(|| Error::Usage("--id".into()))?,
        dir: dir.ok_or_else(|| Error::Usage("--dir".into()))?,
        kind,
        owner,
        idle: nexo_dbg::IDLE,
    })
}

fn pick_session(dir: &std::path::Path, want: &Option<String>) -> Result<String, Error> {
    if let Some(s) = want {
        return Ok(s.clone());
    }
    let live = nexo_dbg::sessions(dir);
    match live.len() {
        1 => Ok(live[0].0.clone()),
        0 => {
            Err(Error::Fail("no debug session: start one with `nexo dbg run <exe>` or `nexo dbg attach <pid>`".into()))
        }
        _ => Err(Error::Fail(format!(
            "more than one debug session is live ({}): pass -s <id>",
            live.iter().map(|s| s.0.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

fn shell_words(w: &[String]) -> String {
    w.iter().map(|x| nexo_bin_shell(x)).collect::<Vec<_>>().join(" ")
}

fn nexo_bin_shell(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-:+=,@%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn print(mut out: Output, id: &str, words: &[String], o: &Opts, head: Option<String>, state: &str) -> String {
    out.command = format!("nexo dbg -s {id} {}", shell_words(words));
    let page = out.page_as(o.from, o.lines.max(MIN_LINES), o.json);
    if o.json {
        let mut v = page.value;
        if let Some(m) = v.as_object_mut() {
            m.insert("session".into(), json!(id));
            m.insert("state".into(), json!(state));
            m.insert("text".into(), json!(page.lines.join("\n")));
        }
        return serde_json::to_string_pretty(&v).unwrap_or_default();
    }
    let mut text = String::new();
    if let Some(h) = head {
        text.push_str(&h);
        text.push('\n');
    }
    text.push_str(&page.text());
    text
}

fn execute(a: &DbgArgs) -> Result<String, Error> {
    let o = options(&a.words)?;
    let dir = nexo_dbg::state_dir();
    let Some(cmd) = o.words.first().map(String::as_str) else {
        return Ok(print(nexo_dbg::help(), "-", &["help".to_string()], &o, None, ""));
    };
    match cmd {
        "__serve" => {
            let start = serve_args(&o.words[1..])?;
            let code = nexo_dbg::serve(&start);
            std::process::exit(code);
        }
        "run" | "attach" => {
            let kind = if cmd == "run" {
                let Some(exe) = o.words.get(1) else { return Err(Error::Usage("run <exe> [args]".into())) };
                let args = o.words[2..].iter().filter(|w| *w != "--").cloned().collect();
                Kind::Run { exe: PathBuf::from(exe), args }
            } else {
                let pid =
                    o.words.get(1).and_then(|p| p.parse().ok()).ok_or_else(|| Error::Usage("attach <pid>".into()))?;
                Kind::Attach(pid)
            };
            let me = std::env::current_exe().map_err(|e| Error::Fail(e.to_string()))?;
            let id = nexo_dbg::spawn(&me, &dir, &kind)?;
            let r = nexo_dbg::request(&dir, &id, &["__first".to_string()])?;
            let head = format!("session {id}");
            Ok(print(r.out, &id, &["status".to_string()], &o, Some(head), &r.state))
        }
        "sessions" => {
            let l = nexo_dbg::sessions(&dir);
            if l.is_empty() {
                return Ok("no debug sessions".into());
            }
            Ok(l.iter().map(|(id, exe, pid)| format!("{id}  pid {pid}  {exe}")).collect::<Vec<_>>().join("\n"))
        }
        "end" => {
            let id = pick_session(&dir, &o.session)?;
            nexo_dbg::request(&dir, &id, &["__end".to_string()])?;
            Ok(format!("session {id} ended"))
        }
        _ => {
            let id = pick_session(&dir, &o.session)?;
            let r = nexo_dbg::request(&dir, &id, &o.words)?;
            Ok(print(r.out, &id, &o.words, &o, None, &r.state))
        }
    }
}

pub fn run(a: &DbgArgs) -> i32 {
    match execute(a) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("nexo dbg: {e}");
            e.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn our_options_are_taken_out_of_the_command() {
        let o = options(&words("-s abc bt 5 --lines 20 --json")).unwrap();
        assert_eq!(o.session.as_deref(), Some("abc"));
        assert_eq!(o.lines, 20);
        assert!(o.json);
        assert_eq!(o.words, words("bt 5"));
    }

    #[test]
    fn a_programs_own_options_are_left_alone() {
        let o = options(&words("run ./p --json --lines 3")).unwrap();
        assert!(!o.json);
        assert_eq!(o.words, words("run ./p --json --lines 3"));
    }
}
