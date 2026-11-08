//! A labelled set for the hindsight trace: hand-written conversations where the truth is known (which tool
//! results the agent really used afterwards, which it did not), and the precision and recall of the trace on
//! it. `cargo test -p nexo-curator hindsight_labels -- --nocapture` prints the table.

use serde_json::{json, Value};

use crate::hindsight::{Interner, Tracer, UKind};

enum S {
    /// the person writes
    U(&'static str),
    /// the agent answers in text
    A(&'static str),
    /// the agent thinks
    T(&'static str),
    /// the agent calls a tool (id, name, input)
    C(&'static str, &'static str, Value),
    /// the result of a call (id, text)
    R(&'static str, String),
}
use S::{A, C, R, T, U};

struct Case {
    name: &'static str,
    steps: Vec<S>,
    /// ids of the results the agent used later; every other result is one it did not
    used: Vec<&'static str>,
}

fn numbered(text: &str) -> String {
    text.lines().enumerate().map(|(i, l)| format!("{:>6}\t{l}\n", i + 1)).collect()
}

fn read(id: &'static str, path: &str) -> S {
    C(id, "read", json!({ "file_path": path }))
}

fn bash(id: &'static str, cmd: &str) -> S {
    C(id, "bash", json!({ "command": cmd }))
}

const STORE: &str = "import time

class SessionStore:
    def __init__(self, ttl_seconds):
        self.ttl_seconds = ttl_seconds
        self._entries = {}

    def put(self, key, value):
        self._entries[key] = (value, time.time() + self.ttl_seconds)

    def purge_expired(self, now):
        dead = [k for k, e in self._entries.items() if e[1] < now]
        for k in dead:
            del self._entries[k]
        return len(dead)
";

const HELPERS: &str = "def pick(items, default):
    if not items:
        return default
    return items[0]

def first_word(text):
    parts = text.split()
    return parts[0] if parts else None
";

const OTHER: &str = "def pick_last(items, default):
    if not items:
        return default
    return items[-1]
";

fn cases() -> Vec<Case> {
    let store = numbered(STORE);
    vec![
        // ---- the agent used the result
        Case {
            name: "quotes a method name",
            steps: vec![
                U("Why do old sessions stay in memory?"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                A("The method purge_expired removes dead entries, but nothing ever calls it."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "edit old_string of a few common words",
            steps: vec![
                U("Make pick return None on empty input"),
                read("r1", "src/helpers.py"),
                R("r1", HELPERS.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "src/helpers.py", "old_string": "    if not items:\n        return default\n    return items[0]", "new_string": "    return items[0] if items else None"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "edit old_string with identifiers",
            steps: vec![
                U("Fix the cleanup"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "src/store.py", "old_string": "        for k in dead:\n            del self._entries[k]", "new_string": "        self._entries = {k: v for k, v in self._entries.items() if k not in dead}"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "quotes a sentence of plain prose",
            steps: vec![
                U("How do I install it?"),
                read("r1", "README.md"),
                R("r1", "# Setup\n\nRun the installer with administrator rights before the first launch, then restart the machine.\nSee the troubleshooting page for more.\n".into()),
                A("The readme says to run the installer with administrator rights before the first launch."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "path and line number",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", store.clone()),
                A("The expiry check is wrong: see src/store.py:12."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "line N of path",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", store.clone()),
                A("Look at line 12 of src/store.py, the comparison runs the wrong way."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "symbol in prose",
            steps: vec![
                U("Why is the retry slow?"),
                read("r1", "src/net.js"),
                R("r1", "export function computeBackoffDelay(attempt) {\n  return 2 ** attempt * 100;\n}\n".into()),
                A("computeBackoffDelay doubles the wait on every attempt, so the fifth retry waits 3.2 seconds."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "error text copied",
            steps: vec![
                U("Run the tests"),
                bash("b1", "pytest -q"),
                R("b1", "FAILED test_color.py::test_pick - AssertionError: expected 'blue_widget' got 'red_widget'".into()),
                A("The test expects blue_widget but the code returns red_widget."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "number copied",
            steps: vec![
                U("How big is the table?"),
                bash("b1", "sqlite3 db 'select count(*) from rows'"),
                R("b1", "48213".into()),
                A("The table holds 48213 rows."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "version copied",
            steps: vec![
                U("Which version is installed?"),
                bash("b1", "tool --version"),
                R("b1", "tool 2.14.7 (build 9)".into()),
                A("You run 2.14.7, which predates the fix."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "path from a grep result reaches a read",
            steps: vec![
                U("Where is the retry logic?"),
                bash("b1", "grep -rn retry crates"),
                R("b1", "crates/net/src/backoff.rs:44:pub fn retry_with(policy: &Policy)\n".into()),
                read("r1", "crates/net/src/backoff.rs"),
                R("r1", "pub fn retry_with(policy: &Policy) {}\n".into()),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "host name copied",
            steps: vec![
                U("Where does it connect?"),
                read("r1", "config.toml"),
                R("r1", "[db]\nhost = \"pg-main.internal.corp\"\n".into()),
                bash("b1", "ping pg-main.internal.corp"),
                R("b1", "64 bytes from 10.0.0.4".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "port in a later command",
            steps: vec![
                U("Check the service"),
                bash("b1", "cat run.env"),
                R("b1", "PORT=48213\nMODE=prod\n".into()),
                bash("b2", "curl -s localhost:48213/health"),
                R("b2", "healthy".into()),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "file name from a listing",
            steps: vec![
                U("Open the invoice"),
                bash("b1", "ls reports"),
                R("b1", "invoice_2024_q3.csv\nnotes.txt\n".into()),
                bash("b2", "head reports/invoice_2024_q3.csv"),
                R("b2", "id,total".into()),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "rare lowercase identifier",
            steps: vec![
                U("What does the helper do?"),
                read("r1", "src/util.py"),
                R("r1", "def frobnicate(widget):\n    return widget.twist()\n".into()),
                A("Calling frobnicate(w) just twists the widget and returns it."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "only the second of two results is used",
            steps: vec![
                U("Look around"),
                read("r1", "docs/a.md"),
                R("r1", "Weekly meeting notes about the picnic and the budget.".into()),
                read("r2", "src/store.py"),
                R("r2", STORE.into()),
                A("purge_expired is the only cleanup path."),
            ],
            used: vec!["r2"],
        },
        Case {
            name: "re-read of the same file, the second is used",
            steps: vec![
                U("Check the store"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                A("Let me read it again."),
                read("r2", "src/store.py"),
                R("r2", STORE.into()),
                A("purge_expired never touches _entries when dead is empty."),
            ],
            used: vec!["r2"],
        },
        Case {
            name: "file changed by a tool, re-read, new line quoted",
            steps: vec![
                U("Add a limit"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                bash("b1", "make regen"),
                R("b1", "regenerated".into()),
                read("r2", "src/store.py"),
                R("r2", STORE.replace("self._entries = {}", "self._entries = {}\n        self.max_entries_allowed = 500")),
                A("Now max_entries_allowed is set in the constructor."),
            ],
            used: vec!["r2"],
        },
        Case {
            name: "identical output in two results, the latest is used",
            steps: vec![
                U("List both folders"),
                bash("b1", "ls a"),
                R("b1", "quarterly_summary.xlsx\nbudget_final.xlsx\n".into()),
                bash("b2", "ls b"),
                R("b2", "quarterly_summary.xlsx\nbudget_final.xlsx\n".into()),
                A("Both folders hold budget_final.xlsx."),
            ],
            used: vec!["b2"],
        },
        Case {
            name: "test names from pytest output",
            steps: vec![
                U("What fails?"),
                bash("b1", "pytest -q"),
                R("b1", "FAILED tests/test_login.py::test_login_expired\nFAILED tests/test_login.py::test_logout_twice\n2 failed in 0.4s".into()),
                A("Two tests fail: test_login_expired and test_logout_twice."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "name given by the person, body from the result",
            steps: vec![
                U("Fix handleSubmit so it ignores empty forms"),
                read("r1", "src/form.js"),
                R("r1", "function handleSubmit(evt) {\n  evt.preventDefault();\n  sendPayload(collectFields(evt.target));\n}\n".into()),
                C("e1", "edit", json!({"file_path": "src/form.js", "old_string": "  sendPayload(collectFields(evt.target));", "new_string": "  const fields = collectFields(evt.target);\n  if (fields.length) sendPayload(fields);"})),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "json key reused",
            steps: vec![
                U("Tune the client"),
                bash("b1", "cat settings.json"),
                R("b1", "{\"retry_budget\": 7, \"verbose\": false}".into()),
                C("e1", "edit", json!({"file_path": "settings.json", "old_string": "\"retry_budget\": 7", "new_string": "\"retry_budget\": 3"})),
                R("e1", "ok".into()),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "quote with different indentation",
            steps: vec![
                U("Explain the cleanup"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                A("It builds the list with dead = [k for k, e in self._entries.items() if e[1] < now] and then deletes each key."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "partial quote from the middle of a line",
            steps: vec![
                U("What does the policy say?"),
                read("r1", "POLICY.md"),
                R("r1", "Employees may carry over at most five unused holiday days into the following calendar year, subject to approval.\n".into()),
                A("Per the policy: at most five unused holiday days into the following calendar year."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "used in thinking only",
            steps: vec![
                U("Why does it crash?"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                T("The method purge_expired iterates _entries while deleting, which is fine because dead is a copy."),
                A("It does not crash there."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "hex value copied",
            steps: vec![
                U("What is compared?"),
                bash("b1", "objdump -d a.out"),
                R("b1", "  40114b:\t3d 7a 1d b6 2f       \tcmp    $0x2fb61d7a,%eax\n".into()),
                A("The key is compared with 0x2fb61d7a."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "edit file named by a result, content of a long line",
            steps: vec![
                U("Rename the flag"),
                bash("b1", "grep -rn enable_turbo_mode ."),
                R("b1", "./app/flags.py:3:enable_turbo_mode = os.environ.get('TURBO') == '1'\n".into()),
                C("e1", "edit", json!({"file_path": "app/flags.py", "old_string": "enable_turbo_mode = os.environ.get('TURBO') == '1'", "new_string": "turbo = os.environ.get('TURBO') == '1'"})),
                R("e1", "ok".into()),
            ],
            used: vec!["b1"],
        },
        // ---- hard cases found by reading what real sessions write (labelled by reading the evidence)
        // the reply writes a windows path with backslashes for a file the call named with slashes
        Case {
            name: "windows path with backslashes, line reference",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("The comparison is inverted at src\\store.py:12."),
            ],
            used: vec!["r1"],
        },
        // the file was read through a windows path; the edit names it with slashes
        Case {
            name: "file read with a windows path, edited with slashes",
            steps: vec![
                U("Tidy the helper"),
                read("r1", "C:\\work\\app\\src\\helpers.py"),
                R("r1", HELPERS.into()),
                read("r2", "C:\\work\\app\\src\\other.py"),
                R("r2", OTHER.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "src/helpers.py", "old_string": "    parts = text.split()\n    return parts[0] if parts else None", "new_string": "    return (text.split() or [None])[0]"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        // a compiler style reference with a column: the line is 12, not the column
        Case {
            name: "line and column reference past the end of the file",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/helpers.py"),
                R("r1", numbered(HELPERS)),
                A("The error is at src/helpers.py:40:3."),
            ],
            used: vec![],
        },
        Case {
            name: "line and column reference inside the file",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("The error is at src/store.py:12:9."),
            ],
            used: vec!["r1"],
        },
        // the agent writes a number with thousands separators
        Case {
            name: "number copied with thousands separators",
            steps: vec![
                U("How big is the table?"),
                bash("b1", "sqlite3 db 'select count(*) from rows'"),
                R("b1", "4821350".into()),
                A("The table holds 4,821,350 rows."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "number in the result has separators, reply has none",
            steps: vec![
                U("How many users?"),
                bash("b1", "report --users"),
                R("b1", "active users: 1,204,331\n".into()),
                A("There are 1204331 active users."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "small numbers with separators are not names",
            steps: vec![
                U("How many?"),
                bash("b1", "report --count"),
                R("b1", "items: 1,024\nlimit: 2,048\n".into()),
                A("There are about 1,000 items and a limit near 2,000."),
            ],
            used: vec![],
        },
        // names with accents and prose in other languages
        Case {
            name: "accented identifier copied",
            steps: vec![
                U("Qué hace la función?"),
                read("r1", "src/util.py"),
                R("r1", "def calcular_año_fiscal(fecha):\n    return fecha.year + 1\n".into()),
                A("calcular_año_fiscal suma uno al año de la fecha."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "spanish sentence quoted",
            steps: vec![
                U("Qué dice la guía?"),
                read("r1", "GUIA.md"),
                R("r1", "Antes de la migración hay que reiniciar el servidor principal y vaciar la caché.\n".into()),
                A("La guía indica que antes de la migración hay que reiniciar el servidor principal."),
            ],
            used: vec!["r1"],
        },
        // markdown escapes the underscores of a name
        Case {
            name: "escaped underscore name",
            steps: vec![
                U("What cleans the entries?"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                A("The method purge\\_expired removes them."),
            ],
            used: vec!["r1"],
        },
        // negative: a reply that mentions a file only through a stock word of a listing
        Case {
            name: "uuid copied",
            steps: vec![
                U("Which build is running?"),
                bash("b1", "tool status"),
                R("b1", "build id 3f2a9c1e-7b44-4d0a-9e21-5c8d1b6a0f77 running".into()),
                A("The running build is 3f2a9c1e-7b44-4d0a-9e21-5c8d1b6a0f77."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "ip address copied",
            steps: vec![
                U("Where does it listen?"),
                bash("b1", "ip addr"),
                R("b1", "inet 192.168.14.37/24 brd 192.168.14.255".into()),
                A("It listens on 192.168.14.37."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "line range with an en dash",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("Lines 10–14 of src/store.py hold the bug."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "line reference from a diff header style",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("The bad check is in src/store.py (line 12)."),
            ],
            used: vec!["r1"],
        },
        // ---- the agent did not use the result
        Case {
            name: "common words by chance",
            steps: vec![
                U("Summarise the notes"),
                read("r1", "notes.txt"),
                R("r1", "The response was slow and the request queue grew; the value dropped after the first minute.".into()),
                A("The response looks fine and every request has a value."),
            ],
            used: vec![],
        },
        Case {
            name: "camel case word known from the person",
            steps: vec![
                U("Add an isEmpty check to the form"),
                bash("b1", "eslint src"),
                R("b1", "src/list.js: 3 warnings (isEmpty is deprecated)".into()),
                A("I will add the isEmpty check now."),
            ],
            used: vec![],
        },
        Case {
            name: "path mentioned, content never used",
            steps: vec![
                U("Look at the code"),
                read("r1", "src/alpha.rs"),
                R("r1", "fn zeta_quadrant() { /* internal */ }\n".into()),
                A("I read src/alpha.rs and it is fine, moving on to src/beta.rs."),
            ],
            used: vec![],
        },
        Case {
            name: "short tokens",
            steps: vec![
                U("Check the row"),
                bash("b1", "sqlite3 db 'select * from t'"),
                R("b1", "id=1 ok no 7".into()),
                A("The row with id 1 is ok, no problem."),
            ],
            used: vec![],
        },
        Case {
            name: "words the agent wrote itself in its own command",
            steps: vec![
                U("Build it"),
                bash("b1", "cargo build -p nexo_engine_core"),
                R("b1", "   Compiling nexo_engine_core v0.1.0\n    Finished dev".into()),
                A("The nexo_engine_core build is done."),
            ],
            used: vec![],
        },
        Case {
            name: "big read ignored",
            steps: vec![
                U("Fix the typo in the title"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                read("r2", "docs/title.md"),
                R("r2", "# Titel of the page\n".into()),
                C("e1", "edit", json!({"file_path": "docs/title.md", "old_string": "# Titel of the page", "new_string": "# Title of the page"})),
                R("e1", "ok".into()),
            ],
            used: vec!["r2"],
        },
        Case {
            name: "old_string from a different read",
            steps: vec![
                U("Change pick in helpers"),
                read("r1", "src/helpers.py"),
                R("r1", HELPERS.into()),
                read("r2", "src/other.py"),
                R("r2", OTHER.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "src/helpers.py", "old_string": "    if not items:\n        return default", "new_string": "    if not items:\n        return None"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "old_string from a different read, read later",
            steps: vec![
                U("Change pick_last in other"),
                read("r2", "src/other.py"),
                R("r2", OTHER.into()),
                read("r1", "src/helpers.py"),
                R("r1", HELPERS.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "src/other.py", "old_string": "    if not items:\n        return default", "new_string": "    if not items:\n        return None"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r2"],
        },
        Case {
            name: "year and tiny numbers",
            steps: vec![
                U("When was it built?"),
                bash("b1", "stat build"),
                R("b1", "Modify: 2024-03-01 10:22\nSize: 4096\nLinks: 12".into()),
                A("It was made in 2024, and I count 12 links, roughly."),
            ],
            used: vec![],
        },
        Case {
            name: "ubiquitous file names",
            steps: vec![
                U("Set up the project"),
                bash("b1", "ls"),
                R("b1", "Cargo.toml\nREADME.md\nsrc\n".into()),
                A("The project has the usual files, nothing unusual in Cargo.toml or README.md, I think."),
            ],
            used: vec![],
        },
        Case {
            name: "ubiquitous identifiers",
            steps: vec![
                U("Look at the class"),
                read("r1", "src/model.py"),
                R("r1", "class Model:\n    def __init__(self):\n        self.user_id = None\n".into()),
                A("A new class with __init__ and user_id would work for the other module."),
            ],
            used: vec![],
        },
        Case {
            name: "boilerplate imports in a new file",
            steps: vec![
                U("Write a script"),
                read("r1", "tools/a.py"),
                R("r1", "import os\nimport sys\nimport re\nimport json\n\nprint('a')\n".into()),
                C("w1", "write", json!({"file_path": "tools/b.py", "content": "import os\nimport sys\nimport re\nimport json\n\nprint('b')\n"})),
                R("w1", "ok".into()),
            ],
            used: vec![],
        },
        Case {
            name: "prose that shares a few words",
            steps: vec![
                U("Reply to Dana"),
                read("r1", "mail.txt"),
                R("r1", "the value of the field is not set in the new form, please check it soon".into()),
                A("Dear Dana, the value of the field must be set before the review."),
            ],
            used: vec![],
        },
        Case {
            name: "line number beyond the file",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", store.clone()),
                A("Something is off at src/store.py:9999."),
            ],
            used: vec![],
        },
        Case {
            name: "line number without a path",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", store),
                A("Check line 12 of the other module."),
            ],
            used: vec![],
        },
        Case {
            name: "stub result",
            steps: vec![
                U("Make the folder"),
                bash("b1", "mkdir out"),
                R("b1", "(no output)".into()),
                A("The folder is ready and the output is clean."),
            ],
            used: vec![],
        },
        Case {
            name: "failed command, different command next",
            steps: vec![
                U("Run the thing"),
                bash("b1", "run_thing --fast"),
                R("b1", "error: unknown flag --fast for run_thing".into()),
                bash("b2", "run_thing"),
                R("b2", "ran".into()),
            ],
            used: vec![],
        },
        Case {
            name: "word only in an earlier result and in the person's text",
            steps: vec![
                U("Rename quarterlyTotals everywhere"),
                read("r1", "src/a.js"),
                R("r1", "const quarterlyTotals = sum(rows);\n".into()),
                A("I will rename quarterlyTotals now."),
            ],
            used: vec![],
        },
        Case {
            name: "a value the person gave, confirmed by a result (no proof in the words)",
            steps: vec![
                U("The service listens on port 48213, check it"),
                bash("b1", "ss -ltn"),
                R("b1", "LISTEN 0 128 0.0.0.0:48213".into()),
                A("Port 48213 is open."),
            ],
            used: vec![],
        },
        Case {
            name: "directory listing not used afterwards",
            steps: vec![
                U("What is in the project?"),
                bash("b1", "ls -la"),
                R("b1", "adapter_registry.py\nbootstrap_loader.py\ncheckpoint_writer.py\n".into()),
                A("Let me think about what you asked instead and answer in general terms."),
            ],
            used: vec![],
        },
        Case {
            name: "edit with an absolute path of a file read with a relative one",
            steps: vec![
                U("Tidy the helper"),
                read("r1", "src/helpers.py"),
                R("r1", HELPERS.into()),
                read("r2", "src/other.py"),
                R("r2", OTHER.into()),
                C(
                    "e1",
                    "edit",
                    json!({"file_path": "/home/dev/app/src/helpers.py", "old_string": "    parts = text.split()\n    return parts[0] if parts else None", "new_string": "    return (text.split() or [None])[0]"}),
                ),
                R("e1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "file read with cat, then edited",
            steps: vec![
                U("Change the title"),
                bash("b1", "cat docs/title.md"),
                R("b1", "# Titel of the page\n\nSome body text lives down here.\n".into()),
                C("e1", "edit", json!({"file_path": "docs/title.md", "old_string": "# Titel of the page", "new_string": "# Title of the page"})),
                R("e1", "ok".into()),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "line reference with only the file name",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("The comparison in store.py:12 is inverted."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "line reference in the L12 style",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("See src/store.py#L12 for the inverted comparison."),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "compiler message quoted in lower case",
            steps: vec![
                U("Build it"),
                bash("b1", "cargo build"),
                R("b1", "error[E0502]: cannot borrow `list` as mutable because it is also borrowed as immutable\n".into()),
                A("The build says it cannot borrow list as mutable because it is also borrowed as immutable, so I will clone first."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "stock phrase in a mail",
            steps: vec![
                U("Answer the customer"),
                read("r1", "inbox/mail1.txt"),
                R("r1", "Thanks for the update. Please let me know if you have any questions about the invoice.\n".into()),
                A("Dear Sam, the refund is on its way. Please let me know if you have any questions about the refund."),
            ],
            used: vec![],
        },
        Case {
            name: "line reference to a file that was not read",
            steps: vec![
                U("Find the bug"),
                read("r1", "src/store.py"),
                R("r1", numbered(STORE)),
                A("The error is probably in src/other.py:3."),
            ],
            used: vec![],
        },
        Case {
            name: "edit of a file never read, lines also in another read",
            steps: vec![
                U("Add the same guard elsewhere"),
                read("r1", "src/helpers.py"),
                R("r1", HELPERS.into()),
                C("w1", "write", json!({"file_path": "src/zzz.py", "content": "def zzz(items, default):\n    if not items:\n        return default\n"})),
                R("w1", "ok".into()),
            ],
            used: vec![],
        },
        Case {
            name: "names copied into a new file",
            steps: vec![
                U("Write a test for the store"),
                read("r1", "src/store.py"),
                R("r1", STORE.into()),
                C("w1", "write", json!({"file_path": "tests/test_store.py", "content": "def test_it():\n    s = SessionStore(5)\n    assert s.purge_expired(99) == 0\n"})),
                R("w1", "ok".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "setting from a config used in a command",
            steps: vec![
                U("Run it verbosely"),
                read("r1", "config.yml"),
                R("r1", "log:\n  level: debug_verbose\n  sink: stderr\n".into()),
                bash("b1", "tool --level debug_verbose"),
                R("b1", "running".into()),
            ],
            used: vec!["r1"],
        },
        Case {
            name: "number from a table row",
            steps: vec![
                U("Report the sales"),
                bash("b1", "report --table"),
                R("b1", "| region | units |\n| north  | 14201 |\n| south  | 9 |\n".into()),
                A("The north sold 14201 units."),
            ],
            used: vec!["b1"],
        },
        Case {
            name: "word of a first result repeated in a second result, only the second named",
            steps: vec![
                U("Compare the two logs"),
                bash("b1", "cat a.log"),
                R("b1", "worker pool_exhausted at 10:01".into()),
                bash("b2", "cat b.log"),
                R("b2", "worker pool_exhausted at 10:09, retrying".into()),
                A("Both logs show pool_exhausted."),
            ],
            used: vec!["b2"],
        },
        Case {
            name: "a result quoted much later",
            steps: vec![
                U("Read the config"),
                read("r1", "config.toml"),
                R("r1", "[cache]\nmax_bytes_resident = 52428800\n".into()),
                A("Noted."),
                U("Now something else, what is 2+2?"),
                A("Four."),
                U("Back to the config: what was the cache limit?"),
                A("max_bytes_resident is 52428800."),
            ],
            used: vec!["r1"],
        },
    ]
}

fn run(case: &Case) -> Vec<(String, bool, bool)> {
    let mut int = Interner::default();
    let mut tr = Tracer::default();
    let mut ids: Vec<&str> = Vec::new();
    for s in &case.steps {
        let m = match s {
            U(t) => json!({"role": "user", "content": [{"type": "text", "text": t}]}),
            A(t) => json!({"role": "assistant", "content": [{"type": "text", "text": t}]}),
            T(t) => json!({"role": "assistant", "content": [{"type": "thinking", "thinking": t}]}),
            C(id, name, input) => {
                json!({"role": "assistant", "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]})
            }
            R(id, t) => {
                ids.push(id);
                json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": t}]})
            }
        };
        tr.add(m, &mut int);
    }
    let results: Vec<&crate::hindsight::Unit> = tr.units.iter().filter(|u| u.kind == UKind::Result).collect();
    assert_eq!(results.len(), ids.len(), "{}", case.name);
    results
        .iter()
        .zip(ids)
        .map(|(u, id)| (format!("{}/{id}", case.name), case.used.contains(&id), !u.uses.is_empty()))
        .collect()
}

#[test]
fn the_labelled_set_is_large_enough() {
    assert!(cases().len() >= 40);
}

#[test]
fn hindsight_labels_precision_and_recall() {
    let (mut tp, mut fp, mut fneg, mut tn) = (0, 0, 0, 0);
    let (mut fps, mut fns) = (Vec::new(), Vec::new());
    for case in cases() {
        for (name, truth, got) in run(&case) {
            match (truth, got) {
                (true, true) => tp += 1,
                (false, true) => {
                    fp += 1;
                    fps.push(name)
                }
                (true, false) => {
                    fneg += 1;
                    fns.push(name)
                }
                (false, false) => tn += 1,
            }
        }
    }
    let precision = tp as f64 / (tp + fp).max(1) as f64;
    let recall = tp as f64 / (tp + fneg).max(1) as f64;
    println!("hindsight labels: tp {tp} fp {fp} fn {fneg} tn {tn} | precision {precision:.3} recall {recall:.3}");
    println!("false positives: {fps:?}");
    println!("false negatives: {fns:?}");
    assert!(fps.is_empty() && fns.is_empty(), "wrong labels: fp {fps:?} fn {fns:?}");
}
