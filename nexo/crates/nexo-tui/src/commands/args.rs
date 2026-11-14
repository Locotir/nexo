//! The argument values the palette offers for a command.

/// The argument values a command offers in the palette that never change:
/// `(value, what it does)`. The model, effort and theme lists come from the app.
pub fn fixed_args(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "permissions" => &[
            ("ask", "ask before writes and commands"),
            ("accept-edits", "writes go through; ask before commands"),
            ("plan", "reads, then a plan to approve before any write"),
            ("yolo", "never ask"),
            ("read-only", "only reads and searches"),
        ],
        "sandbox" => &[
            ("off", "commands run with your own rights"),
            ("workspace-write", "read everything, write only the project and temp"),
            ("read-only", "read everything, write nothing"),
        ],
        "prompt" => &[("edit", "open the system prompt in $VISUAL or $EDITOR")],
        "memory" => &[
            ("user", "your own instructions, for every project"),
            ("add", "write one line into the project's instructions"),
            ("share", "bring in what another session knows"),
        ],
        "settings" => &[
            ("mouse", "mouse capture on or off"),
            ("timestamps", "the time on each message on or off"),
            ("vim", "the vim keys in the editor on or off"),
        ],
        "context" => &[("live", "a live map of the context beside the transcript")],
        "copy" => &[("code", "the last code block of the last reply")],
        "goal" => &[
            ("pause", "hold the goal: nothing more starts for it"),
            ("resume", "go on with the goal"),
            ("off", "clear the goal"),
        ],
        "fast" => &[("on", "use the fast version"), ("off", "back to the base model")],
        "plan" => &[("off", "leave plan mode")],
        "budget" => &[("off", "no limit for this session")],
        "jobs" => &[("kill", "stop every background job (or `kill <id>`)")],
        _ => &[],
    }
}

/// Whether the command's argument is one of a known set: Enter runs the
/// selected value. Elsewhere the list is only a suggestion and Enter sends what was typed.
pub fn closed_args(name: &str) -> bool {
    matches!(name, "permissions" | "effort" | "theme" | "prompt" | "memory" | "copy" | "resume")
}

/// Whether the palette offers values for the command's argument at all.
pub fn takes_listed_args(name: &str) -> bool {
    matches!(name, "model" | "effort" | "theme" | "resume" | "rename" | "export" | "msg" | "add-dir")
        || !fixed_args(name).is_empty()
}
