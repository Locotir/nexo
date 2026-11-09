//! Binary debugging: four stripped programs the agent opens with gdb, objdump and readelf.
//!
//! `dbg-core`: a crash left a core file and the input is gone; say which record the program was holding.
//! `dbg-heap`: a pool audit aborted; say which slot is damaged and what overwrote it.
//! `dbg-corrupt`: a ledger gets one account wrong; say which note corrupted it and how.
//! `dbg-uaf`: a session store shows another customer's total; say which close left the stale entry.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::{compile_c, crash_core, have, keyed, line, linux_only};
use crate::check::{run, Outcome};
use crate::rng::Rng;
use crate::tasks::{files, Task};

fn ready() -> Result<(), String> {
    linux_only("the debugger tasks")?;
    have(&["cc", "gdb", "objdump", "readelf"])
}

fn task(
    id: &'static str,
    trap: &'static str,
    list: &[(&str, &str)],
    turns: Vec<(u32, String)>,
    check: fn(&Path) -> Outcome,
    solution: fn(&Path),
    setup: fn(&Path) -> Result<(), String>,
) -> Task {
    Task {
        id,
        trap,
        group: "heavy2",
        files: files(list),
        turns,
        check,
        solution,
        setup: Some(setup),
        sealed: false,
        mask: &[],
        ready,
    }
}

pub fn tasks() -> Vec<Task> {
    vec![core_task(), heap_task(), corrupt_task(), uaf_task()]
}

const LIMITS: &str = "LIMITS OF YOUR TOOLS
  - every command runs on its own and is stopped after its timeout; drive gdb with -batch and
    -ex (or a command file, -x)
  - a command's output is cut to its first and last 15000 characters: write big outputs to a
    file and read the parts you need
";

// ---------------------------------------------------------------- dbg-core

const RELAY_C: &str = r#"#include <stdio.h>
typedef struct { unsigned id, qty, cents, lot; } Rec;
typedef int (*H)(Rec *);
static int ok(Rec *r) { return (int)(r->qty + r->cents); }
static H routes[1024];
static Rec ring[8];
static unsigned rn, nread;
static Rec cur;
static unsigned route_of(const Rec *r) { return (r->id * 2654435761u ^ r->qty * 40503u ^ r->lot) & 1023u; }
int main(int argc, char **argv) {
  FILE *f; unsigned a, b, c, d; volatile int sink = 0;
  if (argc < 2 || !(f = fopen(argv[1], "r"))) return 2;
  for (int i = 0; i < 1024; i++) routes[i] = ok;
  routes[POISON] = 0;
  while (fscanf(f, "%u %u %u %u", &a, &b, &c, &d) == 4) {
    cur.id = a; cur.qty = b; cur.cents = c; cur.lot = d; nread++;
    sink += routes[route_of(&cur)](&cur);
    ring[rn++ % 8] = cur;
  }
  return sink & 1;
}
"#;

type Rec = [u32; 4];

fn route(r: &Rec) -> u32 {
    (r[0].wrapping_mul(2654435761) ^ r[1].wrapping_mul(40503) ^ r[3]) & 1023
}

fn rec(r: &mut Rng) -> Rec {
    [r.int(10_000, 99_999) as u32, r.int(1, 999) as u32, r.int(1, 99_999) as u32, r.int(100_000, 9_999_999) as u32]
}

/// The records, the index of the one that crashes the program, and the route nobody else takes.
fn relay_world() -> (Vec<Rec>, usize, u32) {
    const N: usize = 300;
    const K: usize = 228;
    let mut r = Rng::new(0x5e1a7);
    let mut v: Vec<Rec> = (0..N).map(|_| rec(&mut r)).collect();
    loop {
        let poison = route(&v[K]);
        if v[..K].iter().all(|x| route(x) != poison) {
            return (v, K, poison);
        }
        v[K] = rec(&mut r);
    }
}

fn setup_core(wd: &Path) -> Result<(), String> {
    let (v, _, poison) = relay_world();
    let input: String = v.iter().map(|r| format!("{} {} {} {}\n", r[0], r[1], r[2], r[3])).collect();
    crash_core(wd, "relay", &RELAY_C.replace("POISON", &poison.to_string()), "records.txt", &input)
}

fn core_task() -> Task {
    let rules = format!(
        "RELAY - rules of the task

GOAL
  ./relay crashed while it was reading a file of records. The file is gone. Say what the program
  was holding when it died.

WHAT YOU HAVE (and all you have)
  ./relay      the compiled program, stripped; its source code does not exist anywhere you can reach
  core         the core file the crash left (gdb ./relay core opens it)
  This folder is a sealed box: there is no network, and nothing outside this folder belongs to
  the task. The system's tools (sh, gdb, objdump, readelf, strings, cc...) can be used.

WHAT IS KNOWN ABOUT THE PROGRAM
  It was run as: ./relay records.txt. Every line of records.txt had four decimal numbers, in this
  order: id (5 digits), qty, cents, lot. The program keeps the record it is handling and a ring of
  the last 8 records it finished. It sends each record down one of 1024 routes, numbered from 0,
  picked from the record's numbers.

{LIMITS}
THE FINAL ANSWERS
  Plain text, one value per line, numbers in decimal.
  answer.txt, with exactly these lines:
      position: <how many records the program had read when it crashed, counting that one>
      id: <the id of the record it was handling>
      qty: <its qty>
      cents: <its cents>
      lot: <its lot>
      route: <the number of the route it was sent to>
  ring.txt, when the second question is asked: 8 lines, oldest first, each \"<id> <qty> <cents> <lot>\"
  for one of the records the program had finished just before that one.
  When you are sure, stop and reply in a few lines.

HOW IT IS GRADED
  Every value is checked against the input the program really crashed on. Partial answers earn
  partial credit.
",
        LIMITS = LIMITS
    );
    let rules: &'static str = Box::leak(rules.into_boxed_str());
    task(
        "dbg-core",
        "a core file and a stripped binary: the record in flight and the ring of finished ones are spread over \
         registers, globals and the disassembly",
        &[("RULES.md", rules)],
        vec![
            (
                1,
                "./relay crashed and left a core file; the input is gone. I need to know which record it was handling \
                 when it died. Read RULES.md first: it says what you may use and what answer.txt must hold."
                    .into(),
            ),
            (
                1,
                "Now the second question from RULES.md: write ring.txt with the 8 records the program had finished \
                 just before that one, oldest first."
                    .into(),
            ),
        ],
        check_core,
        solution_core,
        setup_core,
    )
}

fn check_core(wd: &Path) -> Outcome {
    let (v, k, poison) = relay_world();
    let a = keyed(wd, "answer.txt");
    let r = v[k];
    let mut parts = vec![
        line(&a, "position", &(k + 1).to_string()),
        line(&a, "id", &r[0].to_string()),
        line(&a, "qty", &r[1].to_string()),
        line(&a, "cents", &r[2].to_string()),
        line(&a, "lot", &r[3].to_string()),
        line(&a, "route", &poison.to_string()),
    ];
    let ring: Vec<Vec<String>> = std::fs::read_to_string(wd.join("ring.txt"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_whitespace().map(String::from).collect())
        .filter(|l: &Vec<String>| !l.is_empty())
        .collect();
    for (i, want) in v[k - 8..k].iter().enumerate() {
        let w: Vec<String> = want.iter().map(u32::to_string).collect();
        parts.push((ring.get(i) == Some(&w), format!("ring line {}: expected {}", i + 1, w.join(" "))));
    }
    Outcome::share(parts)
}

fn solution_core(wd: &Path) {
    let (v, k, poison) = relay_world();
    let r = v[k];
    let _ = std::fs::write(
        wd.join("answer.txt"),
        format!("position: {}\nid: {}\nqty: {}\ncents: {}\nlot: {}\nroute: {poison}\n", k + 1, r[0], r[1], r[2], r[3]),
    );
    let ring: String = v[k - 8..k].iter().map(|r| format!("{} {} {} {}\n", r[0], r[1], r[2], r[3])).collect();
    let _ = std::fs::write(wd.join("ring.txt"), ring);
}

// ---------------------------------------------------------------- dbg-heap

const POOL_C: &str = r#"#include <stdio.h>
#include <stdlib.h>
typedef struct { char name[16]; unsigned size; unsigned magic; } Slot;
static Slot slots[96];
static unsigned njobs;
int main(int argc, char **argv) {
  FILE *f; char buf[96]; unsigned size;
  if (argc < 2 || !(f = fopen(argv[1], "r"))) return 2;
  while (fscanf(f, "%95s %u", buf, &size) == 2) {
    Slot *s = &slots[njobs % 96];
    s->size = size; s->magic = 0xC0FFEE00u + (njobs % 96);
    char *d = s->name; const char *p = buf; while ((*d++ = *p++)) {}
    njobs++;
  }
  for (unsigned i = 0; i < 96; i++)
    if (slots[i].magic != 0xC0FFEE00u + i) { fprintf(stderr, "pool: slot %u corrupt\n", i); abort(); }
  return 0;
}
"#;

/// The jobs, the number of the damaged slot and the 8 characters that overwrote its size and magic.
fn pool_world() -> (Vec<(String, u32)>, usize, String) {
    const N: usize = 300;
    const J: usize = 281;
    let mut r = Rng::new(0xb10c);
    let low = |r: &mut Rng, n: usize| -> String { (0..n).map(|_| (b'a' + r.below(26) as u8) as char).collect() };
    let alnum = |r: &mut Rng, n: usize| -> String {
        let set = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        (0..n).map(|_| set[r.below(set.len())] as char).collect()
    };
    let mut jobs: Vec<(String, u32)> = Vec::new();
    let mut bytes = String::new();
    for i in 0..N {
        let name = if i == J {
            bytes = alnum(&mut r, 8);
            format!("{}{}{}", low(&mut r, 16), bytes, low(&mut r, 6))
        } else {
            let n = r.int(5, 15) as usize;
            low(&mut r, n)
        };
        jobs.push((name, r.int(16, 4096) as u32));
    }
    (jobs, J % 96, bytes)
}

fn setup_heap(wd: &Path) -> Result<(), String> {
    let (jobs, _, _) = pool_world();
    let input: String = jobs.iter().map(|(n, s)| format!("{n} {s}\n")).collect();
    crash_core(wd, "pool", POOL_C, "jobs.txt", &input)
}

fn heap_task() -> Task {
    let rules = format!(
        "POOL - rules of the task

GOAL
  ./pool takes a file of jobs, keeps each one in a fixed slot of a table and, when the file ends,
  audits the table. The audit aborted. One slot is damaged: say which and what damaged it.

WHAT YOU HAVE (and all you have)
  ./pool       the compiled program, stripped; its source code does not exist anywhere you can reach
  core         the core file the abort left (gdb ./pool core opens it)
  The jobs file is gone. This folder is a sealed box: there is no network, and nothing outside it
  belongs to the task. The system's tools (sh, gdb, objdump, readelf, strings, cc...) can be used.

WHAT IS KNOWN ABOUT THE PROGRAM
  It was run as: ./pool jobs.txt. Each line of jobs.txt held a name and a size. A slot is 24 bytes:
  a 16-byte name, a 4-byte size and a 4-byte check word, in that order. Job number n (from 0) goes
  to slot n modulo 96. The audit looks at the check word of every slot.

{LIMITS}
THE FINAL ANSWER
  answer.txt, with exactly these lines:
      jobs: <how many jobs the program had read>
      slot: <the number of the damaged slot, from 0>
      bytes: <the 8 characters that sit where the damaged slot's size and check word should be>
  When you are sure, stop and reply in a few lines.
",
        LIMITS = LIMITS
    );
    let rules: &'static str = Box::leak(rules.into_boxed_str());
    task(
        "dbg-heap",
        "a corrupted table in a core file: the damage is a string that ran past its field, readable only as \
         raw bytes next to the slot",
        &[("RULES.md", rules)],
        vec![(
            1,
            "./pool aborted in its audit and left a core file; the jobs file is gone. I need to know which slot is \
             damaged and what overwrote it. Read RULES.md first: it says what you may use and what answer.txt must \
             hold."
                .into(),
        )],
        check_heap,
        solution_heap,
        setup_heap,
    )
}

fn check_heap(wd: &Path) -> Outcome {
    let (jobs, slot, bytes) = pool_world();
    let a = keyed(wd, "answer.txt");
    Outcome::share(vec![
        line(&a, "jobs", &jobs.len().to_string()),
        line(&a, "slot", &slot.to_string()),
        line(&a, "bytes", &bytes),
    ])
}

fn solution_heap(wd: &Path) {
    let (jobs, slot, bytes) = pool_world();
    let _ = std::fs::write(wd.join("answer.txt"), format!("jobs: {}\nslot: {slot}\nbytes: {bytes}\n", jobs.len()));
}

// ---------------------------------------------------------------- dbg-corrupt

const LEDGER_C: &str = r#"#include <stdio.h>
struct A { long bal; int denied; char tag[8]; long limit; };
static struct A acct[20];
int main(int argc, char **argv) {
  FILE *f; int seq, a; char kind[16], text[64]; long amt;
  if (argc < 2 || !(f = fopen(argv[1], "r"))) return 2;
  for (int i = 0; i < 20; i++) acct[i].limit = 1000;
  while (fscanf(f, "%d %d %15s %ld %63s", &seq, &a, kind, &amt, text) == 5) {
    struct A *x = &acct[a];
    if (kind[0] == 'd') x->bal += amt;
    else if (kind[0] == 'w') { if (x->bal - amt >= -x->limit) x->bal -= amt; else x->denied++; }
    else { char *d = x->tag; const char *p = text; while ((*d++ = *p++)) {} }
  }
  for (int i = 0; i < 20; i++) printf("account %d balance %ld denied %d\n", i, acct[i].bal, acct[i].denied);
  return 0;
}
"#;

struct Ledger {
    ops: Vec<String>,
    /// The line of the note that damages account 13.
    note: usize,
    /// The limit of account 13 after the damage.
    limit: i64,
    /// What account 13 should end with: balance and denied withdrawals.
    right: (i64, i32),
}

/// Plays account 13's operations; `damage` is the line and limit value the note sets.
fn play(ops: &[(usize, i64, char)], damage: Option<(usize, i64)>) -> (i64, i32) {
    let (mut bal, mut denied, mut limit) = (0i64, 0, 1000i64);
    for &(line, amt, kind) in ops {
        if let Some((l, v)) = damage {
            if line >= l && limit == 1000 {
                limit = v;
            }
        }
        match kind {
            'd' => bal += amt,
            _ if bal - amt >= -limit => bal -= amt,
            _ => denied += 1,
        }
    }
    (bal, denied)
}

fn ledger_world() -> Ledger {
    for seed in 1..400u64 {
        let mut r = Rng::new(seed);
        let (mut ops, mut mine) = (Vec::new(), Vec::new());
        let note_at = 300usize;
        let (c12, c13) = (b'a' + r.below(26) as u8, b'a' + r.below(26) as u8);
        let text: String =
            (0..12).map(|_| (b'a' + r.below(26) as u8) as char).chain([c12 as char, c13 as char]).collect::<String>();
        let limit = c12 as i64 + 256 * c13 as i64;
        for i in 1..=420usize {
            let seq = i;
            if i == note_at {
                ops.push(format!("{seq} 13 note 0 {text}"));
                continue;
            }
            if i == 150 {
                ops.push(format!("{seq} 7 note 0 {}", "decoyxxxxx"));
                continue;
            }
            let a = if r.chance(0.12) { 13 } else { r.below(20) };
            if a == 7 && r.chance(0.2) {
                ops.push(format!("{seq} 7 note 0 tag{}", r.below(90)));
                continue;
            }
            let dep = r.chance(0.5);
            let amt = if dep { r.int(300, 3000) } else { r.int(500, 5000) };
            if a == 13 {
                mine.push((i, amt, if dep { 'd' } else { 'w' }));
            }
            ops.push(format!("{seq} {a} {} {amt} -", if dep { "deposit" } else { "withdraw" }));
        }
        let right = play(&mine, None);
        let bug = play(&mine, Some((note_at, limit)));
        if right != bug && right.1 >= 2 {
            return Ledger { ops, note: note_at, limit, right };
        }
    }
    panic!("no seed gives a ledger the damage changes");
}

fn setup_corrupt(wd: &Path) -> Result<(), String> {
    compile_c(LEDGER_C, &wd.join("ledger"), &["-O0", "-s", "-U_FORTIFY_SOURCE"])
}

fn corrupt_task() -> Task {
    let w = ledger_world();
    let rules: &'static str = Box::leak(
        format!(
            "LEDGER - rules of the task

GOAL
  ./ledger ops.txt prints the balance and the denied withdrawals of 20 accounts. Account 13 comes
  out wrong. Find what damages it.

WHAT YOU HAVE (and all you have)
  ./ledger     the compiled program, stripped; its source code does not exist anywhere you can reach
  ops.txt      the operations it was run on: sequence, account, kind (deposit, withdraw, note),
               amount, text (\"-\" when the kind has none)
  This folder is a sealed box: there is no network, and nothing outside it belongs to the task. The
  system's tools (sh, gdb, objdump, readelf, strings, cc...) can be used.

THE RULES OF THE LEDGER
  Every account starts at 0 with an overdraft limit of 1000. A withdrawal is accepted when the
  balance after it is not below minus the limit; otherwise it is denied and counted. A note stores
  its text in the account's tag.

{LIMITS}
THE FINAL ANSWER
  answer.txt, with exactly these lines:
      line: <the line of ops.txt that damages account 13>
      field: <the field of the account 13 record it damages: bal, denied, tag or limit>
      value: <the value that field holds afterwards, in decimal>
  When you are sure, stop and reply in a few lines.
",
            LIMITS = LIMITS
        )
        .into_boxed_str(),
    );
    let ops: &'static str = Box::leak(w.ops.join("\n").into_boxed_str());
    let ops: &'static str = Box::leak(format!("{ops}\n").into_boxed_str());
    task(
        "dbg-corrupt",
        "a text copy that runs past a field of a struct and rewrites the neighbouring one: found by comparing \
         the program's layout with the operations",
        &[("RULES.md", rules), ("ops.txt", ops)],
        vec![(
            1,
            format!(
                "./ledger ops.txt gets account 13 wrong: by the rules, account 13 should end with balance {} and {} \
                 denied withdrawals. Read RULES.md first; write the three facts for account 13 in answer.txt.",
                w.right.0, w.right.1
            ),
        )],
        check_corrupt,
        solution_corrupt,
        setup_corrupt,
    )
}

fn check_corrupt(wd: &Path) -> Outcome {
    let w = ledger_world();
    let a = keyed(wd, "answer.txt");
    Outcome::share(vec![
        line(&a, "line", &w.note.to_string()),
        line(&a, "field", "limit"),
        line(&a, "value", &w.limit.to_string()),
    ])
}

fn solution_corrupt(wd: &Path) {
    let w = ledger_world();
    let _ = std::fs::write(wd.join("answer.txt"), format!("line: {}\nfield: limit\nvalue: {}\n", w.note, w.limit));
}

// ---------------------------------------------------------------- dbg-uaf

const SESSIONS_C: &str = r#"#include <stdio.h>
typedef struct { int id; long total; } S;
static S pool[64];
static S *freel[64];
static int nfree;
static S *tab[1000];
int main(int argc, char **argv) {
  FILE *f; char op[16]; int id; long v;
  if (argc < 2 || !(f = fopen(argv[1], "r"))) return 2;
  for (int i = 63; i >= 0; i--) freel[nfree++] = &pool[i];
  while (fscanf(f, "%15s %d", op, &id) == 2) {
    if (op[0] == 'o') { S *s = freel[--nfree]; s->id = id; s->total = 0; tab[id] = s; }
    else if (op[0] == 'a') { if (fscanf(f, "%ld", &v) == 1) tab[id]->total += v; }
    else if (op[0] == 's') printf("show %d: total %ld\n", id, tab[id]->total);
    else { freel[nfree++] = tab[id]; if (id % 16 != 5) tab[id] = 0; }
  }
  return 0;
}
"#;

struct Uaf {
    ops: Vec<String>,
    /// The line of the close that leaves the stale entry, and the line of the show that reads it.
    close: usize,
    show: usize,
    stale: u32,
    other: u32,
    wrong: i64,
    right: i64,
}

fn uaf_world() -> Uaf {
    let mut r = Rng::new(0x0afb);
    let (s, t) = (37u32, 612u32);
    let mut ops: Vec<String> = vec![format!("open {s}"), format!("add {s} 400")];
    let mut live: Vec<u32> = Vec::new();
    let mut used = vec![s, t];
    let mut phase = |ops: &mut Vec<String>, live: &mut Vec<u32>, steps: usize, r: &mut Rng| {
        for _ in 0..steps {
            let x = r.below(100);
            if (x < 22 && live.len() < 30) || live.len() < 4 {
                let id = loop {
                    let id = r.int(1, 999) as u32;
                    if id % 16 != 5 && !used.contains(&id) {
                        break id;
                    }
                };
                used.push(id);
                live.push(id);
                ops.push(format!("open {id}"));
            } else if x < 62 {
                let id = *r.pick(live);
                ops.push(format!("add {id} {}", r.int(10, 900)));
            } else if x < 85 {
                let id = *r.pick(live);
                ops.push(format!("show {id}"));
            } else if live.len() > 6 {
                let i = r.below(live.len());
                ops.push(format!("close {}", live.remove(i)));
            }
        }
    };
    phase(&mut ops, &mut live, 330, &mut r);
    ops.push(format!("close {s}"));
    let close = ops.len();
    ops.push(format!("open {t}"));
    ops.push(format!("add {t} 9100"));
    ops.push(format!("show {t}"));
    phase(&mut ops, &mut live, 60, &mut r);
    ops.push(format!("show {s}"));
    let show = ops.len();
    Uaf { ops, close, show, stale: s, other: t, wrong: 9100, right: 400 }
}

fn setup_uaf(wd: &Path) -> Result<(), String> {
    let bin = wd.join("sessions");
    compile_c(SESSIONS_C, &bin, &["-O0", "-s"])?;
    // the design must hold: the stale show prints the other session's total
    let w = uaf_world();
    let got = run(Command::new(&bin).arg(wd.join("ops.txt")), None, Duration::from_secs(20));
    let want = format!("show {}: total {}", w.stale, w.wrong);
    if got.stdout.lines().any(|l| l == want) {
        Ok(())
    } else {
        Err(format!("the sessions program did not show the stale total ({want}): {}", got.tail(300)))
    }
}

fn uaf_task() -> Task {
    let w = uaf_world();
    let rules: &'static str = Box::leak(
        format!(
            "SESSIONS - rules of the task

GOAL
  ./sessions ops.txt keeps customer sessions in a small pool of 64 objects and prints the total of a
  session when asked. One total it prints belongs to another customer. Find the operation that
  leaves the program reading an object it has already given back.

WHAT YOU HAVE (and all you have)
  ./sessions   the compiled program, stripped; its source code does not exist anywhere you can reach
  ops.txt      the operations it was run on: open ID, add ID AMOUNT, show ID, close ID
  This folder is a sealed box: there is no network, and nothing outside it belongs to the task. The
  system's tools (sh, gdb, objdump, readelf, strings, cc...) can be used.

WHAT IS KNOWN ABOUT THE PROGRAM
  open takes an object from the pool's free list, close puts it back; the next open gets the object
  that went back last. A session table maps an id to its object.

{LIMITS}
THE FINAL ANSWER
  answer.txt, with exactly these lines:
      close: <the line of ops.txt of the close that leaves the table pointing at a returned object>
      show: <the line of ops.txt of the show that prints the wrong total>
      other: <the id of the session whose total it printed instead>
  When you are sure, stop and reply in a few lines.
",
            LIMITS = LIMITS
        )
        .into_boxed_str(),
    );
    let ops: &'static str = Box::leak(format!("{}\n", w.ops.join("\n")).into_boxed_str());
    task(
        "dbg-uaf",
        "a use-after-free with no crash: the symptom is a wrong number far from the cause, and the cause is a \
         condition on the id inside the binary",
        &[("RULES.md", rules), ("ops.txt", ops)],
        vec![(
            1,
            format!(
                "./sessions ops.txt printed `show {}: total {}` but session {} only ever got {}. Read RULES.md first; \
                 write the three facts in answer.txt.",
                w.stale, w.wrong, w.stale, w.right
            ),
        )],
        check_uaf,
        solution_uaf,
        setup_uaf,
    )
}

fn check_uaf(wd: &Path) -> Outcome {
    let w = uaf_world();
    let a = keyed(wd, "answer.txt");
    Outcome::share(vec![
        line(&a, "close", &w.close.to_string()),
        line(&a, "show", &w.show.to_string()),
        line(&a, "other", &w.other.to_string()),
    ])
}

fn solution_uaf(wd: &Path) {
    let w = uaf_world();
    let _ =
        std::fs::write(wd.join("answer.txt"), format!("close: {}\nshow: {}\nother: {}\n", w.close, w.show, w.other));
}
