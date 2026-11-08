//! Test runs and their failures (engine::fixes).

use super::*;

/// What `fixes` says of a session that edits `src/calc.rs` between test runs, each run a command
/// and its output.
fn fixes_of(runs: &[(&str, &str)]) -> String {
    let mut msgs = vec![json!({"role": "user", "content": "Make the test suite pass."})];
    for (k, (cmd, out)) in runs.iter().enumerate() {
        msgs.extend(step(2 * k + 1, cmd, out));
        msgs.extend(step(2 * k + 2, &format!("sed -i 's/a{k}/b{k}/' src/calc.rs"), ""));
    }
    let mut e = Engine::new(params("nudge=0").unwrap());
    e.sync(&msgs);
    e.fixes(0, msgs.len(), Some(msgs.len()))
}

const FAILING_RUN: &str = "running 2 tests\ntest calc::test_round ... FAILED\nfailures:\n    calc::test_round\ntest result: FAILED. 1 passed; 1 failed";

const PASSING_RUN: &str = "running 2 tests\ntest result: ok. 2 passed; 0 failed";

#[test]
fn narrower_passing_run_does_not_mark_earlier_failures_fixed() {
    for (broad, narrow) in
        [("cargo test", "cargo test unrelated"), ("npm test", "npm test -- -t other"), ("pytest", "pytest tests/b.py")]
    {
        let said = fixes_of(&[(broad, FAILING_RUN), (narrow, PASSING_RUN)]);
        assert!(!said.contains("now pass"), "{narrow}: {said}");
        assert!(!said.contains("made the failing tests pass"), "{narrow}: {said}");
        assert!(said.contains("tests failing in the last run in them: test_round"), "{narrow}: {said}");
    }
    // the same command passing, or a broader one, still marks them fixed
    for (failing, passing) in [
        ("cargo test", "cargo test"),
        ("cargo test calc", "cargo test"),
        ("cd app && npm test 2>&1 | tail -40", "npm test > out.txt; cat out.txt"),
    ] {
        let said = fixes_of(&[(failing, FAILING_RUN), (passing, PASSING_RUN)]);
        assert!(said.contains("the tests that failed and now pass: test_round"), "{said}");
        assert!(said.contains("the changes that made the failing tests pass: src/calc.rs"), "{said}");
        assert!(!said.contains("failing in the last run"), "{said}");
    }
}

#[test]
fn test_that_fails_again_is_not_listed_as_now_passing() {
    let said = fixes_of(&[("cargo test", FAILING_RUN), ("cargo test", PASSING_RUN), ("cargo test", FAILING_RUN)]);
    assert!(!said.contains("now pass"), "{said}");
    assert!(said.contains("tests failing in the last run in them: test_round"), "{said}");
}

/// Failing runs of several ecosystems, each piped so that the command exits 0 (no `is_error`), with the
/// test the run names (empty: the output names none in a shape the curator reads).
const FAILING_RUNS: [(&str, &str, &str); 12] = [
        ("go test ./... 2>&1 | tail -20", "--- FAIL: TestAdd (0.00s)\n    calc_test.go:9: got 3, want 4\nFAIL\nexit status 1\nFAIL\texample/calc\t0.002s", "TestAdd"),
        ("npx mocha | tail -5", "  2 passing (8ms)\n  1 failing\n\n  1) Cart\n       adds item:\n     expected 3 to equal 4", ""),
        ("bundle exec rspec | tail -8", "Finished in 0.01 seconds\n3 examples, 1 failure\n\nFailed examples:\n\nrspec ./spec/cart_spec.rb:12 # Cart adds item", "./spec/cart_spec.rb:12"),
        ("npx jest 2>&1 | tail -9", "FAIL src/cart.test.js\n  ● Cart › adds item\n\n    expect(received).toBe(expected)\n\nTests:       1 failed, 2 passed, 3 total", "Cart › adds item"),
        ("cargo test 2>&1 | tail -6", "test parser::parses_empty ... FAILED\n\nfailures:\n    parser::parses_empty\n\ntest result: FAILED. 4 passed; 1 failed", "parses_empty"),
        ("./gradlew test | tail -4", "CalcTest > testAdd() FAILED\n    org.opentest4j.AssertionFailedError at CalcTest.java:9\n3 tests completed, 1 failed", "testAdd"),
        ("mvn test | tail -4", "Tests run: 3, Failures: 1, Errors: 0, Skipped: 0\n[INFO] BUILD FAILURE", ""),
        ("vendor/bin/phpunit | tail -3", "FAILURES!\nTests: 3, Assertions: 3, Failures: 1.", ""),
        ("dotnet test | tail -2", "Failed!  - Failed:     1, Passed:     2, Skipped:     0, Total:     3", ""),
        ("mix test | tail -2", "Finished in 0.03 seconds\n3 tests, 1 failure", ""),
        ("ctest | tail -2", "67% tests passed, 1 tests failed out of 3", ""),
        ("node --test | tail -3", "not ok 2 - adds item\n# pass 2\n# fail 1", "adds item"),
    ];

/// The passing output of each of those commands, in the same order.
const PASSING_RUNS: [&str; 12] = [
    "ok  \texample/calc\t0.002s",
    "  3 passing (8ms)",
    "Finished in 0.01 seconds\n3 examples, 0 failures",
    "PASS src/cart.test.js\nTests:       3 passed, 3 total",
    "test result: ok. 5 passed; 0 failed",
    "BUILD SUCCESSFUL in 2s",
    "Tests run: 3, Failures: 0, Errors: 0, Skipped: 0\n[INFO] BUILD SUCCESS",
    "OK (3 tests, 3 assertions)",
    "Passed!  - Failed:     0, Passed:     3, Skipped:     0, Total:     3",
    "3 tests, 0 failures",
    "100% tests passed, 0 tests failed out of 3",
    "# pass 3\n# fail 0",
];

#[test]
fn failing_runs_of_any_ecosystem_are_not_read_as_passing() {
    for ((cmd, out, name), pass) in FAILING_RUNS.iter().zip(PASSING_RUNS) {
        // a failing run, an edit, the same failure again: nothing passes
        let said = fixes_of(&[(cmd, out), (cmd, out)]);
        assert!(!said.contains("now pass"), "{cmd}: {said}");
        assert!(!said.contains("made the failing tests pass"), "{cmd}: {said}");
        // a passing run after it does mark it fixed, with the test it named
        let said = fixes_of(&[(cmd, out), (cmd, pass)]);
        assert!(said.contains("the changes that made the failing tests pass: src/calc.rs"), "{cmd}: {said}");
        if !name.is_empty() {
            assert!(said.contains(&format!("the tests that failed and now pass: {name}")), "{cmd}: {said}");
            let open = fixes_of(&[(cmd, out)]);
            assert!(open.contains(&format!("tests failing in the last run in them: {name}")), "{cmd}: {open}");
        }
    }
    // a count the run's own script prints next to another number is a label, not a failure
    let said = fixes_of(&[("pytest", FAILING_RUN), ("pytest; ./check", "3 passed\nrows 20 failures 0\nerrors: 0")]);
    assert!(said.contains("the tests that failed and now pass: test_round"), "{said}");
}

#[test]
fn passing_runs_that_name_errors_or_exit_codes_are_passing() {
    // test names, package paths and logs of a passing run that hold a failure word and a number
    let passing = [
            "ok  \tgithub.com/acme/api/errors\t1.204s\nok  \tgithub.com/acme/api/fail\t0.3s",
            "PASS src/api.test.js\n  ✓ returns error 404 for an unknown id (5 ms)\n  ✓ returns error 422\nTests:       2 passed, 2 total",
            "  ✓ exits with exit code 2 on bad args\n  ✓ prints usage on exit code 1\n  2 passing (4ms)",
            "  returns error 404 for an unknown id\n  2 passing (9ms)",
            "INFO HTTP error 503, retrying\nWARN upstream error: 502 from cache\n3 passed in 0.2s",
            "Cart\n  reports failed 2 items as skipped\n2 examples, 0 failures",
            "  ✓ rejects when the worker exited with code 1 (3 ms)\nTests:       4 passed, 4 total",
            "✔ reports a build that failed with exit code 2 (1.2ms)\nℹ tests 1\nℹ pass 1\nℹ fail 0",
            "ok 3 - process exited with code 1 is shown\nPASS job finished with exit code 1 is retried",
            "[info] Passed: Total 2, Failed 0, Errors 0, Passed 2",
            "Tests: succeeded 2, failed 0, canceled 0, ignored 0, pending 0",
        ];
    for out in passing {
        assert!(!TEST_FAILED.is_match(out), "{out}");
        assert!(!ERROR_MARKS.is_match(out), "{out}");
    }
    let said = fixes_of(&[(FAILING_RUNS[0].0, FAILING_RUNS[0].1), (FAILING_RUNS[0].0, passing[0])]);
    assert!(said.contains("the tests that failed and now pass: TestAdd"), "{said}");
    let said = fixes_of(&[("npx jest", FAILING_RUNS[3].1), ("npx jest", passing[1])]);
    assert!(said.contains("the tests that failed and now pass: Cart › adds item"), "{said}");
    // failure counts and exit codes as runners and hosts print them still read as failing
    for out in FAILING_RUNS.iter().map(|r| r.1).chain([
        FAILING_RUN,
        "Tests run: 3, Failures: 1, Errors: 0",
        "Tests: 3, Assertions: 3, Failures: 1.",
        "Test summary: total: 3, failed: 1, succeeded: 2",
        "Total tests: 3\n     Passed: 2\n     Failed: 1",
        "Ran 3 tests in 0.01s\n\n(failures=1)",
        "Tests failed: 1, passed: 2",
        "# pass 2\n# fail 1",
        "ℹ tests 3\nℹ pass 2\nℹ fail 1",
        "✖ adds item (2.1ms)\n  AssertionError: expected 3 to equal 4",
        "[error] Failed: Total 2, Failed 1, Errors 0, Passed 1",
        "Tests: succeeded 1, failed 1, canceled 0, ignored 0, pending 0",
        "ok 1 - a\n\n[exit code 1]",
        "Exit code: 2\nWall time: 1.2 seconds",
        "error Command failed with exit code 1.",
        "make: *** [Makefile:4: test] Error 2",
        "exit status 1",
    ]) {
        assert!(TEST_FAILED.is_match(out), "{out}");
    }
}

#[test]
fn test_commands_of_common_runners_are_test_runs() {
    for cmd in [
        "./gradlew test",
        "gradle test",
        "python manage.py test",
        "bin/rails test",
        "bundle exec rake spec",
        "flutter test",
        "php artisan test",
        "npx playwright test",
        "cargo nextest run",
        "node --test",
        "sbt test",
        "bazel test //...",
    ] {
        assert!(TEST_COMMAND.is_match(cmd), "{cmd}");
    }
    for cmd in ["cat test.txt", "ls tests", "git commit -m 'test'", "echo test"] {
        assert!(!TEST_COMMAND.is_match(cmd), "{cmd}");
    }
}
