use super::*;

pub(super) fn marathon_turns() -> Vec<String> {
    let [first, _, audit, last] = turns();
    vec![
        first,
        "Implement docs/spec/01-restock.md. Add tests for it to tests/planning.rs and make sure all the tests pass."
            .into(),
        audit,
        "Implement docs/spec/02-reserve.md, with tests in a new file tests/reserve.rs; all the tests must pass.".into(),
        "Implement docs/spec/03-tonnes.md, with tests in a new file tests/tonnes.rs; all the tests must pass.".into(),
        "Implement docs/spec/04-markdown.md, with tests in a new file tests/markdown.rs; all the tests must pass."
            .into(),
        "Implement docs/spec/05-bulk.md, with tests in a new file tests/bulk.rs; all the tests must pass.".into(),
        "Run `cargo run --quiet --bin audit` and the tests again and fix whatever they report.".into(),
        last,
    ]
}

pub(super) fn marathon() -> Task {
    let mut files = crate_files();
    files.extend(specs());
    Task {
        id: "marathon",
        trap: "nine requests in one session: three bugs, an audit, five specifications to read and implement (with \
               tables of examples), then values and facts from the very start; the history grows well past a \
               curated view",
        group: "long",
        files,
        turns: marathon_turns().into_iter().map(|t| (1, t)).collect(),
        check: marathon_check,
        solution: marathon_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

/// The marathon with twenty times the rows of examples in each specification: reading them takes
/// the history well past 200k tokens, so a summary at 100k or 200k falls in the middle of the work
/// and the last request still needs what the first one found.
pub(super) fn marathon_heavy() -> Task {
    let mut files = crate_files();
    files.extend(specs_scaled(20));
    Task {
        id: "marathon-heavy",
        trap: "the marathon with twenty times the examples in each specification: the history grows past 200k \
               tokens, so a summary falls in the middle of the work",
        group: "heavy",
        files,
        turns: marathon_turns().into_iter().map(|t| (1, t)).collect(),
        check: marathon_check,
        solution: marathon_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}
