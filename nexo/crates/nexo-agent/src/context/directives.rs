//! The curator's efficiency directives, in the two wordings the model is given.

/// The curator's efficiency directives: with the curator's view the first four
/// took 41 % off the time to a solution and 23 % off the calls (the second
/// and fourth are bounded: a range read never stands in for the material a
/// task has to reason about, and brevity holds for changes, not for answers;
/// a partial read of a large file and a bare-array answer each lost an
/// objective in a measured run); the sixth asks
/// for the `Finding:` and `Plan:` notes (the `note` tool) the curator keeps with
/// the first message once their messages leave the view; the seventh for the
/// `Rule:` notes it keeps for the whole session, beside the user's own standing
/// instructions. Without a `note` tool the same directives ask for the lines
/// in the reply, where the curator reads them as well: in a measured run a
/// request for a tool that was not offered went unanswered for the whole task.
macro_rules! directives {
    ($keep:literal, $rule:literal) => {
        concat!("[curator] Working efficiently: (1) when several tool calls do not depend on \
each other (reading or listing files, searching, running independent checks), make them all in the same response; \
(2) do not read again what is already in this conversation; search and line ranges are for reaching the part of a \
large file you need, never for skipping material the task has to reason about: read everything the answer depends \
on before concluding, since a conclusion drawn from a partial read is worth less than the tokens it saved; (3) make \
your edits, then run the checks once, not after every small step; (4) when a change is done, reply in at most three \
short lines: what changed, in which files, and why; do not repeat code or file contents; when the request is a \
question or an analysis, answer it in full, the result stated plainly and in the form asked for (JSON when JSON was \
asked); (5) do not cut the output of a test run or a build with head or tail: a failure hidden by the cut cannot \
be recalled later. (6) When a result settles something (a value, a formula, an address, a format, the cause of an \
error), keep it ", $keep, ": one line beginning with \"Finding:\"; keep your current plan as one \
line beginning with \"Plan:\" and update it when it changes. Older messages leave this context, but these lines \
are kept with the first message, in your words: that block (marked [curator]) is where to look before reading or \
running something again. (7) When the user states a preference, a constraint or a standing instruction, keep it \
", $rule, ": one line beginning with \"Rule:\" so it holds for the rest of the session; follow every rule \
listed in this context (marked [curator]), the later one when two conflict.")
    };
}

/// The directives when the model has a `note` tool.
pub const CURATOR_DIRECTIVES: &str = directives!("with the `note` tool", "with `note`");
/// The directives when it has not: the notes are lines of its reply.
pub const CURATOR_DIRECTIVES_SAID: &str =
    directives!("as the first line of your next reply", "as a line of your reply");
