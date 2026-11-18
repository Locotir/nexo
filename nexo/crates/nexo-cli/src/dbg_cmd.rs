//! `nexo dbg`: run a program under a debugger, or attach to one, and drive it one bounded call at a time.
//! The program lives in a small server (see the `nexo-dbg` crate) between two calls.

use std::path::PathBuf;

use clap::Args;
use nexo_dbg::{Error, Host, Kind};

#[derive(Args, Debug)]
pub struct DbgArgs {
    /// `run <exe> [args]`, `attach <pid>`, `sessions`, `end`, or a command for a live session (`help` lists them).
    /// Also: `-s <id>` picks a session when more than one is live; `--lines N`, `--from X`, `--grep RE`, `--json`
    /// as in `nexo bin`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., value_name = "COMMAND")]
    pub words: Vec<String>,
}

fn serve_args(w: &[String]) -> Result<nexo_dbg::Start, Error> {
    let (mut id, mut dir, mut owner, mut run, mut attach) = (None, None, None, None, None);
    let (mut core, mut exe): (Option<PathBuf>, Option<PathBuf>) = (None, None);
    let mut remote: Option<String> = None;
    let mut opts = nexo_dbg::LaunchOpts::default();
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
            "--env-stdin" => {
                // The starter sends the values on stdin so they never show in a process listing.
                let mut text = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                    .map_err(|e| Error::Fail(format!("cannot read the environment from the starter: {e}")))?;
                opts.env.extend(nexo_dbg::decode_env(&text)?);
            }
            "--unset" => opts.unset.push(v.clone()),
            "--cwd" => opts.cwd = Some(PathBuf::from(v)),
            "--aslr" => opts.aslr = v == "on",
            "--argv0" => opts.argv0 = Some(v.clone()),
            "--attach" => attach = v.parse().ok(),
            "--core" => core = Some(PathBuf::from(v)),
            "--exe" => exe = Some(PathBuf::from(v)),
            "--remote" => remote = Some(v.clone()),
            other => return Err(Error::Usage(format!("unknown server option {other}"))),
        }
    }
    let kind = match (run, attach, core) {
        _ if remote.is_some() => Kind::Remote { addr: remote.unwrap_or_default(), exe },
        (Some(exe), _, _) if opts.is_default() => Kind::Run { exe, args },
        (Some(exe), _, _) => Kind::RunWith { exe, args, opts },
        (None, Some(pid), _) => Kind::Attach(pid),
        (None, None, Some(file)) => Kind::Core { file, exe },
        _ => return Err(Error::Usage("the server needs --run, --attach or --core".into())),
    };
    Ok(nexo_dbg::Start {
        id: id.ok_or_else(|| Error::Usage("--id".into()))?,
        dir: dir.ok_or_else(|| Error::Usage("--dir".into()))?,
        kind,
        owner,
        idle: nexo_dbg::IDLE,
    })
}

/// What `serve --gdb <port>` serves: a program started with the launch options before it, or a pid.
fn gdb_kind(rest: &[String], usage: impl Fn() -> Error) -> Result<Kind, Error> {
    // Launch options come before the program, as in `run`.
    let mut opts = nexo_dbg::LaunchOpts::default();
    let mut at = 0;
    while let Some(flag) = rest.get(at).map(String::as_str) {
        let val = || rest.get(at + 1).cloned().ok_or_else(&usage);
        match flag {
            "--env" => match val()?.split_once('=') {
                Some((k, v)) if !k.is_empty() => opts.env.push((k.to_string(), v.to_string())),
                _ => return Err(Error::Usage("--env needs NAME=VALUE".into())),
            },
            "--unset" => opts.unset.push(val()?),
            "--cwd" => opts.cwd = Some(PathBuf::from(val()?)),
            "--argv0" => opts.argv0 = Some(val()?),
            "--aslr" => {
                opts.aslr = true;
                at += 1;
                continue;
            }
            _ => break,
        }
        at += 2;
    }
    let rest = &rest[at..];
    let kind = match rest.first().map(String::as_str) {
        Some("--attach") if opts.is_default() => {
            Kind::Attach(rest.get(1).and_then(|p| p.parse().ok()).ok_or_else(usage)?)
        }
        Some("--attach") => {
            return Err(Error::Usage("launch options apply to a program the stub starts, not to --attach".into()))
        }
        Some(exe) => {
            let (exe, args) = (PathBuf::from(exe), rest[1..].iter().filter(|a| *a != "--").cloned().collect());
            if opts.is_default() {
                Kind::Run { exe, args }
            } else {
                Kind::RunWith { exe, args, opts }
            }
        }
        None => return Err(usage()),
    };
    Ok(kind)
}

/// `serve --gdb <port> <program> [args]` or `serve --gdb <port> --attach <pid>`: nexo's debugger as a GDB stub on 127.0.0.1.
fn serve_gdb(w: &[String]) -> Result<String, Error> {
    let usage = || {
        Error::Usage(
            "usage: serve --gdb <port> [--env K=V]... [--unset K]... [--cwd DIR] [--aslr] [--argv0 NAME] <program> [args]  |  serve --gdb <port> --attach <pid>  (port 0: any free port)"
                .into(),
        )
    };
    let mut it = w.iter();
    if it.next().map(String::as_str) != Some("--gdb") {
        return Err(usage());
    }
    let port: u16 = it.next().and_then(|p| p.parse().ok()).ok_or_else(usage)?;
    let rest: Vec<String> = it.cloned().collect();
    let kind = gdb_kind(&rest, usage)?;
    nexo_dbg::serve_gdb(&kind, port, &mut |addr| {
        println!("gdb stub listening on {addr}: connect with `target remote {addr}` or `nexo dbg connect {addr}`");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    })?;
    Ok("gdb stub ended".into())
}

pub(crate) fn execute(a: &DbgArgs) -> Result<String, Error> {
    let call = nexo_dbg::parse(&a.words)?;
    if call.words.first().map(String::as_str) == Some("serve") {
        return serve_gdb(&call.words[1..]);
    }
    nexo_dbg::set_data_dir(nexo_core::paths::data_dir());
    if call.words.first().map(String::as_str) == Some("__serve") {
        let start = serve_args(&call.words[1..])?;
        let code = nexo_dbg::serve(&start);
        std::process::exit(code);
    }
    let me = std::env::current_exe().map_err(|e| Error::Fail(e.to_string()))?;
    nexo_dbg::execute(&call, &nexo_dbg::state_dir(), &Host::Process(me), "nexo dbg")
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

    fn kind(words: &[&str]) -> Result<Kind, Error> {
        let w: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        gdb_kind(&w, || Error::Usage("usage".into()))
    }

    #[test]
    fn the_gdb_stub_takes_launch_options_before_the_program() {
        let Kind::RunWith { exe, args, opts } =
            kind(&["--env", "A=1", "--unset", "B", "--cwd", "/", "--aslr", "--argv0", "x", "./p", "one"]).unwrap()
        else {
            panic!("launch options make a RunWith");
        };
        assert_eq!((exe, args), (PathBuf::from("./p"), vec!["one".to_string()]));
        assert_eq!(opts.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!(
            (opts.unset, opts.cwd, opts.aslr, opts.argv0),
            (vec!["B".into()], Some("/".into()), true, Some("x".into()))
        );
        assert!(matches!(kind(&["./p"]).unwrap(), Kind::Run { .. }));
        assert!(kind(&["--env", "NOEQ", "./p"]).is_err());
        assert!(kind(&["--env", "A=1", "--attach", "5"]).is_err());
        assert!(matches!(kind(&["--attach", "5"]).unwrap(), Kind::Attach(5)));
    }
}
