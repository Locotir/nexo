//! `nexo dbg`: run a program under a debugger, or attach to one, and drive it one bounded call at a time.
//! The program lives in a small server (see the `nexo-dbg` crate) between two calls.

use std::path::PathBuf;

use clap::Args;
use nexo_dbg::{Error, Host, Kind};

#[derive(Args, Debug)]
pub struct DbgArgs {
    /// `run <exe> [args]`, `attach <pid>`, `sessions`, `end`, or a command for a live session (`help` lists them).
    /// Also: `-s <id>` picks a session when more than one is live; `--lines N`, `--from X`, `--json` as in `nexo bin`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., value_name = "COMMAND")]
    pub words: Vec<String>,
}

fn serve_args(w: &[String]) -> Result<nexo_dbg::Start, Error> {
    let (mut id, mut dir, mut owner, mut run, mut attach) = (None, None, None, None, None);
    let (mut core, mut exe): (Option<PathBuf>, Option<PathBuf>) = (None, None);
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
            "--core" => core = Some(PathBuf::from(v)),
            "--exe" => exe = Some(PathBuf::from(v)),
            other => return Err(Error::Usage(format!("unknown server option {other}"))),
        }
    }
    let kind = match (run, attach, core) {
        (Some(exe), _, _) => Kind::Run { exe, args },
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

fn execute(a: &DbgArgs) -> Result<String, Error> {
    let call = nexo_dbg::parse(&a.words)?;
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
