use super::*;

// ---- tools generated from the clap definition of `nexo bin` ----

pub(super) fn bin_command() -> clap::Command {
    crate::cli::Args::command().find_subcommand("bin").expect("bin is a command").clone()
}

/// Arguments that are not tool inputs: clap's own, and the one that prints on stderr.
pub(super) fn hidden_arg(a: &Arg) -> bool {
    matches!(a.get_id().as_str(), "help" | "version" | "timing")
}

pub(super) fn is_flag(a: &Arg) -> bool {
    !a.get_action().takes_values()
}

pub(super) fn is_list(a: &Arg) -> bool {
    matches!(a.get_action(), ArgAction::Append) || a.get_num_args().is_some_and(|r| r.max_values() > 1)
}

pub(super) fn arg_schema(a: &Arg) -> Value {
    let help = a.get_help().map(|h| h.to_string()).unwrap_or_default();
    let mut s = if is_flag(a) {
        json!({"type": "boolean"})
    } else if is_list(a) {
        json!({"type": "array", "items": {"type": "string"}})
    } else if a.get_default_values().first().is_some_and(|d| d.to_str().is_some_and(|d| d.parse::<i64>().is_ok()))
        || a.get_value_parser().type_id() == std::any::TypeId::of::<usize>()
        || a.get_value_parser().type_id() == std::any::TypeId::of::<nexo_bin::Lines>()
    {
        json!({"type": ["integer", "string"]})
    } else {
        json!({"type": "string"})
    };
    let possible: Vec<String> = a.get_possible_values().iter().map(|p| p.get_name().to_string()).collect();
    if !possible.is_empty() && !is_flag(a) && !is_list(a) {
        s["enum"] = json!(possible);
    }
    if let Some(d) = a.get_default_values().first().and_then(|d| d.to_str()) {
        s["default"] = json!(d);
    }
    if !help.is_empty() {
        s["description"] = json!(help);
    }
    s
}

pub(super) fn bin_tools() -> Vec<Tool> {
    bin_command()
        .get_subcommands()
        .filter(|c| c.get_name() != "help")
        .map(|c| {
            let mut props = serde_json::Map::new();
            let mut required = Vec::new();
            for a in c.get_arguments().filter(|a| !hidden_arg(a)) {
                props.insert(a.get_id().to_string(), arg_schema(a));
                if a.is_required_set() {
                    required.push(a.get_id().to_string());
                }
            }
            let about = c.get_about().map(|a| a.to_string()).unwrap_or_default();
            let name = c.get_name();
            let gate = if SAVING_OPS.contains(&name) { " Saves data: needs --allow-changes." } else { "" };
            Tool {
                name: format!("bin_{name}"),
                description: format!("nexo bin {name}: {about}{gate}"),
                schema: json!({"type": "object", "properties": props, "required": required}),
                kind: Kind::Bin(name.to_string()),
            }
        })
        .collect()
}

/// The words after `nexo bin <op>` for these JSON arguments: flags first, then the positionals after `--`.
pub(super) fn bin_argv(op: &str, args: &Value) -> Result<Vec<String>, String> {
    let bin = bin_command();
    let c = bin.find_subcommand(op).ok_or_else(|| format!("no nexo bin op {op:?}"))?;
    let known: Vec<&Arg> = c.get_arguments().filter(|a| !hidden_arg(a)).collect();
    let obj = args.as_object().ok_or("arguments must be an object")?;
    for k in obj.keys() {
        if !known.iter().any(|a| a.get_id() == k.as_str()) {
            let valid: Vec<&str> = known.iter().map(|a| a.get_id().as_str()).collect();
            return Err(format!("bin_{op} has no argument {k:?}; valid: {}", valid.join(", ")));
        }
    }
    let mut argv = vec!["nexo".to_string(), "bin".to_string(), op.to_string()];
    let mut positional: Vec<&Arg> = known.iter().copied().filter(|a| a.is_positional()).collect();
    positional.sort_by_key(|a| a.get_index());
    for a in known.iter().filter(|a| !a.is_positional()) {
        let Some(v) = obj.get(a.get_id().as_str()).filter(|v| !v.is_null()) else { continue };
        let flag = match (a.get_long(), a.get_short()) {
            (Some(l), _) => format!("--{l}"),
            (None, Some(s)) => format!("-{s}"),
            _ => continue,
        };
        if is_flag(a) {
            if v.as_bool().ok_or_else(|| format!("{} must be true or false", a.get_id()))? {
                argv.push(flag);
            }
        } else if is_list(a) {
            for item in list_of(Some(v))? {
                argv.extend([flag.clone(), item]);
            }
        } else {
            let s = scalar(Some(v)).ok_or_else(|| format!("{} must be a string or a number", a.get_id()))?;
            argv.extend([flag, s]);
        }
    }
    argv.push("--".to_string());
    let mut missing: Option<&str> = None;
    for a in positional {
        let id = a.get_id().as_str();
        match obj.get(id).filter(|v| !v.is_null()) {
            None => missing = missing.or(Some(id)),
            Some(v) => {
                if let Some(m) = missing {
                    return Err(format!("{id} was given but {m} (before it) was not; give {m} too"));
                }
                argv.extend(list_of(Some(v))?);
            }
        }
    }
    Ok(argv)
}

// ---- the three debugger tools ----

pub(super) fn dbg_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "dbg_start".into(),
            description: "nexo dbg: start a session that stays alive between calls. kind core (core file or minidump, reading only) always works; run (starts a program, so it executes code), attach (a pid of yours) and connect (GDB stub) need --allow-changes. Then use dbg_command.".into(),
            schema: json!({"type": "object", "properties": {
                "kind": {"type": "string", "enum": ["run", "core", "attach", "connect"]},
                "target": {"type": ["string", "integer"], "description": "program path, core file, pid or host:port"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "program arguments (run)"},
                "main": {"type": "boolean", "description": "run: stop at main"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "run: environment variables to set for the program (NAME to value)"},
                "unset": {"type": "array", "items": {"type": "string"}, "description": "run: environment variable names to take out"},
                "cwd": {"type": "string", "description": "run: the folder the program starts in"},
                "aslr": {"type": "boolean", "description": "run: leave address randomisation on (off by default, so addresses repeat)"},
                "argv0": {"type": "string", "description": "run: what the program sees as its own name, argv[0]"},
                "exe": {"type": "string", "description": "core or connect: the program the core or stub belongs to"}
            }, "required": ["kind", "target"]}),
            kind: Kind::DbgStart,
        },
        Tool {
            name: "dbg_command".into(),
            description: "nexo dbg: one command on the live session, such as `break main`, `cont`, `bt`, `regs`, `mem <addr> 32`, or `do \"break f; cont; bt\"` (`help` lists them). Without --allow-changes only reading commands run; write, setreg, call (and print of an expression that calls a function), name, note, serve, calls, stdin, watch, hooks that set values and any unknown command need --allow-changes.".into(),
            schema: json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "session": {"type": "string", "description": "session id, when more than one is live"},
                "lines": {"type": ["integer", "string"], "description": "most lines in the answer, or all"},
                "from": {"type": "string", "description": "continue a cut answer at this address or position"},
                "json": {"type": "boolean"}
            }, "required": ["command"]}),
            kind: Kind::DbgCommand,
        },
        Tool {
            name: "dbg_stop".into(),
            description: "nexo dbg: end a session and the program under it.".into(),
            schema: json!({"type": "object", "properties": {
                "session": {"type": "string", "description": "session id, when more than one is live"}
            }}),
            kind: Kind::DbgStop,
        },
    ]
}

// ---- the API tool ----

pub(super) fn api_tools() -> Vec<Tool> {
    vec![Tool {
        name: "api".into(),
        description: "nexo api: find the API behind a website. One line: `analyze cap.har --save map.json`, `describe map.json [EP]`, `client map.json --lang ts|py|sh [--out FILE]` or `call map.json EP -p k=v --auth NAME=ENVVAR` (--dry-run first). Secrets go in environment variables and are passed by name (`@env:VAR`); a secret value is refused. Describe, analyze without --save and --dry-run always work; a call, --login and saving a file (also client --out) need --allow-changes. No watch, no --reveal.".into(),
        schema: json!({"type": "object", "properties": {
            "command": {"type": "string", "description": "the nexo api line without the word nexo, for example `call map.json \"GET /v1/items\" --dry-run`"}
        }, "required": ["command"]}),
        kind: Kind::Api,
    }]
}
