//! A compiled game whose coin rule the agent must work out without its source.
//!
//! The agent gets a stripped binary (`realm`), the game's data files and a save. The coins a kill
//! pays come from several places that only make sense together: the enemy's bounty and level
//! (`data/enemies.dat`), the zone's tier (`data/zones.dat`) and the multiplier of that tier (only in
//! the binary), the effects of the equipped items (`data/items.dat`, with codes the binary
//! interprets), the player's level, luck and streak (the save). The data carry decoys (a legacy
//! `gold` column, a `GLD` effect that is not gold, items in the bag) and the manual is out of date.
//! The agent writes `coins.rs` with `loot(save, enemy, zone)` against a provided `harness.rs`; the
//! checker compiles it away from the binary and runs it on 400 cases it has never seen.
//!
//! The answer needs facts gathered over many calls, far apart in the history: the hard case for a
//! curator.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use super::Task;
use crate::check::{run, Outcome};
use crate::rng::Rng;
use crate::seal;

const TIER_PCT: [i64; 5] = [100, 120, 145, 175, 210];
const CLASSES: [&str; 6] = ["beast", "undead", "humanoid", "critter", "construct", "elemental"];
const SLOTS: [&str; 7] = ["head", "body", "hands", "feet", "ring", "trinket", "weapon"];
const SYL: [&str; 20] = [
    "ka", "lor", "vex", "mi", "dra", "thu", "sel", "orn", "ish", "gal", "bry", "zen", "ul", "fae", "mor", "quin",
    "tas", "rho", "ven", "ak",
];
/// How many unseen cases grade the answer.
const CASES: usize = 400;

struct Enemy {
    id: String,
    name: String,
    class: &'static str,
    level: i64,
    hp: i64,
    atk: i64,
    bounty: i64,
    gold: i64,
}

struct Item {
    id: String,
    slot: &'static str,
    weight: i64,
    /// `(code, value)` pairs, written `CODE+value;...` in the data.
    effects: Vec<(&'static str, i64)>,
}

struct Zone {
    id: String,
    tier: i64,
    biome: &'static str,
    danger: i64,
}

struct World {
    enemies: Vec<Enemy>,
    items: Vec<Item>,
    zones: Vec<Zone>,
}

fn name(r: &mut Rng, parts: usize) -> String {
    (0..parts).map(|_| *r.pick(&SYL)).collect()
}

/// The game's data: 180 enemies, 150 items, 16 zones, always the same.
fn world() -> World {
    let mut r = Rng::new(7);
    let mut seen = HashSet::new();
    let mut enemies = Vec::new();
    while enemies.len() < 180 {
        let parts = r.int(2, 3) as usize;
        let n = name(&mut r, parts);
        if !seen.insert(n.clone()) {
            continue;
        }
        let class = *r.pick(&CLASSES);
        let level = r.int(1, 45);
        let kind = *r.pick(&["Warden", "Husk", "Stalker", "Imp", "Golem", "Wisp", "Rat", "Knight"]);
        let title = n[..1].to_uppercase() + &n[1..];
        enemies.push(Enemy {
            name: format!("{title} {kind}"),
            id: n,
            class,
            level,
            hp: 20 + level * r.int(6, 14),
            atk: 2 + level / 2 + r.int(0, 6),
            bounty: r.int(3, 120),
            gold: r.int(1, 400),
        });
    }
    let codes = ["GRD", "LCK", "GLD", "ATK", "DEF", "HP"];
    let mut items = Vec::new();
    while items.len() < 150 {
        let n = name(&mut r, 2)
            + "_"
            + *r.pick(&["ring", "band", "boots", "helm", "charm", "idol", "glove", "mail", "blade"]);
        if !seen.insert(n.clone()) {
            continue;
        }
        let k = r.int(1, 3) as usize;
        let effects = r
            .sample(&codes, k)
            .into_iter()
            .map(|c| {
                let v = match c {
                    "GRD" => r.int(4, 30),
                    "LCK" => r.int(1, 6),
                    "GLD" => r.int(5, 40),
                    "HP" => r.int(5, 50),
                    _ => r.int(1, 9),
                };
                (c, v)
            })
            .collect();
        items.push(Item { id: n, slot: r.pick::<&str>(&SLOTS), weight: r.int(1, 30), effects });
    }
    let mut zones = Vec::new();
    while zones.len() < 16 {
        let n = name(&mut r, 2) + "_" + *r.pick(&["vale", "crypt", "marsh", "spire", "wastes", "hollow"]);
        if !seen.insert(n.clone()) {
            continue;
        }
        zones.push(Zone {
            id: n,
            tier: r.int(0, 4),
            biome: r.pick::<&str>(&["forest", "cave", "swamp", "ruin", "desert"]),
            danger: r.int(1, 9),
        });
    }
    World { enemies, items, zones }
}

fn table(header: &str, rows: impl Iterator<Item = String>) -> String {
    let mut s = header.to_string() + "\n";
    for r in rows {
        s += &r;
        s.push('\n');
    }
    s
}

/// The data files, as the game and the harness read them.
fn data_files(w: &World) -> [(&'static str, String); 3] {
    [
        (
            "data/enemies.dat",
            table(
                "# id|name|class|level|hp|atk|bounty|gold   (gold: legacy field, kept for old saves)",
                w.enemies.iter().map(|e| {
                    format!("{}|{}|{}|{}|{}|{}|{}|{}", e.id, e.name, e.class, e.level, e.hp, e.atk, e.bounty, e.gold)
                }),
            ),
        ),
        (
            "data/items.dat",
            table(
                "# id|slot|weight|effects",
                w.items.iter().map(|i| {
                    let eff: Vec<String> = i.effects.iter().map(|(c, v)| format!("{c}+{v}")).collect();
                    format!("{}|{}|{}|{}", i.id, i.slot, i.weight, eff.join(";"))
                }),
            ),
        ),
        (
            "data/zones.dat",
            table(
                "# id|tier|biome|danger",
                w.zones.iter().map(|z| format!("{}|{}|{}|{}", z.id, z.tier, z.biome, z.danger)),
            ),
        ),
    ]
}

const RULES: &str = "REALM - rules of the task

GOAL
  Find out exactly how many coins the game ./realm pays for a kill, and write coins.rs
  with a function loot(save, enemy, zone) that returns that number.

WHAT YOU HAVE (and all you have)
  ./realm        the compiled game, stripped; its source code does not exist anywhere you can reach
  data/          enemies.dat, items.dat, zones.dat: the game's data
  hero.sav       a save; the format is plain text, key=value per line
  MANUAL.txt     the player manual; it was written for an older version and may be wrong
  harness.rs     the frame your answer plugs into: it defines Save, gives you the data
                 files, and runs loot(); do not change it (the original is used for grading)
  This folder is a sealed box: there is no network, and nothing outside this folder
  belongs to the task. The system's own tools (sh, objdump, gdb, strings, cc, rustc...)
  can be used, but they hold no part of the answer. There is no script interpreter in the box.

YOU MAY
  - run ./realm as often as you like, with any arguments (./realm help lists them)
  - write your own save files and data experiments inside this folder
  - read, disassemble, trace or debug the binary (objdump, gdb, strings...)
  - write any helper programs and notes inside this folder

LIMITS OF YOUR TOOLS
  - every command is stopped after its timeout (120 seconds unless you ask for more, at
    most 600): split long experiments into batches
  - a command's output is cut to its first and last 15000 characters: write big
    outputs to a file and read the parts you need
  - files you write in this folder stay between commands

YOU MAY NOT
  - look for the source code or the answer outside this folder
  - use the network
  - make coins.rs run, load or call the game, or any other program, or read files: no
    std::process, std::fs, std::net, File, include_str!, include_bytes!, unsafe or extern
    in coins.rs; read the data with harness.rs's data(\"enemies.dat\") and friends
  - ask the user anything: work until the task is finished

THE FINAL ANSWER
  coins.rs, in this folder, with:
      use crate::{data, Save};
      pub fn loot(save: &Save, enemy: &str, zone: &str) -> i64
  - save is a save file in the format of hero.sav, already parsed by the harness;
    enemy and zone are ids from data/enemies.dat and data/zones.dat
  - it returns the coins, computed by coins.rs itself from the save and from the
    data, using the Rust standard library only
  - build and try it with:
      rustc --edition 2021 harness.rs -o harness && ./harness hero.sav ENEMY ZONE
  When you are sure, stop and reply with the rule you found, in a few lines. Your last
  version of coins.rs is the answer; nothing else is graded.

HOW IT IS GRADED
  coins.rs is copied to a new folder with the original harness.rs and data/ and nothing
  else (the game is not there), compiled with rustc, and loot() is called on 400 saves,
  enemies and zones you have never seen: any level, luck and streak, and from 0 to 6
  equipped items, sometimes the same item twice. Each answer must equal what the game
  pays, exactly. The score is the share of the 400 that are right; the task is solved
  only at 400 of 400.
  Check your coins.rs against ./realm on many cases of your own before you finish.
";

const MANUAL: &str = "REALM 0.9 - player manual (written for 0.7; some rules changed since)

Run ./realm help for the commands. Fights are automatic: ./realm duel --save hero.sav --enemy ID --zone ID

Coins: every kill pays the enemy's bounty, scaled by how dangerous the zone is.
Items: effects are codes such as ATK+3. Only equipped items count; the bag is storage.
Luck makes good things happen more often.
Streaks: consecutive kills are rewarded.
";

/// The frame the answer plugs into. Reads the data at compile time, so coins.rs needs no file IO;
/// with arguments it prints one answer, without them it answers JSON lines on stdin (the checker).
const HARNESS: &str = r##"//! The frame coins.rs plugs into. Do not change this file: the original is used for grading.
//!
//!   rustc --edition 2021 harness.rs -o harness
//!   ./harness SAVE_FILE ENEMY_ID ZONE_ID      prints the coins loot() returns
//!
//! Without arguments it reads JSON lines {"save": "<save text>", "enemy": "...", "zone": "..."}
//! on stdin and answers each with {"coins": N} or {"error": "..."}.

mod coins;

use std::io::{BufRead, Write};

/// A save file, parsed.
#[derive(Debug, Clone, Default)]
pub struct Save {
    pub name: String,
    pub level: i64,
    pub luck: i64,
    pub streak: i64,
    /// The equipped item ids, in the order of the file (an id may repeat).
    pub equipped: Vec<String>,
    /// The item ids in the bag.
    pub bag: Vec<String>,
}

impl Save {
    /// Parses the key=value lines of a save; missing keys keep their defaults (level 1).
    pub fn parse(text: &str) -> Save {
        let mut s = Save { level: 1, ..Save::default() };
        let list = |v: &str| v.split(',').filter(|x| !x.is_empty()).map(String::from).collect();
        for line in text.lines() {
            let Some((k, v)) = line.trim_end_matches('\r').split_once('=') else { continue };
            match k {
                "name" => s.name = v.to_string(),
                "level" => s.level = v.trim().parse().unwrap_or(0),
                "luck" => s.luck = v.trim().parse().unwrap_or(0),
                "streak" => s.streak = v.trim().parse().unwrap_or(0),
                "equipped" => s.equipped = list(v),
                "bag" => s.bag = list(v),
                _ => {}
            }
        }
        s
    }
}

/// The text of a data file: "enemies.dat", "items.dat" or "zones.dat".
pub fn data(name: &str) -> &'static str {
    match name {
        "enemies.dat" => include_str!("data/enemies.dat"),
        "items.dat" => include_str!("data/items.dat"),
        "zones.dat" => include_str!("data/zones.dat"),
        _ => panic!("no data file {name}"),
    }
}

/// The value of the string field `key` in a flat JSON object.
fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = line[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let (mut out, mut chars) = (String::new(), rest.chars());
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}

fn answer(save: &Save, enemy: &str, zone: &str) -> Result<i64, String> {
    std::panic::catch_unwind(|| coins::loot(save, enemy, zone)).map_err(|e| {
        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 3 {
        let text = std::fs::read_to_string(&args[0]).unwrap_or_else(|e| panic!("{}: {e}", args[0]));
        match answer(&Save::parse(&text), &args[1], &args[2]) {
            Ok(n) => println!("{n}"),
            Err(e) => {
                eprintln!("loot panicked: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if !args.is_empty() {
        eprintln!("usage: harness SAVE_FILE ENEMY_ID ZONE_ID   (or JSON lines on stdin)");
        std::process::exit(2);
    }
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let got = match (field(&line, "save"), field(&line, "enemy"), field(&line, "zone")) {
            (Some(s), Some(e), Some(z)) => answer(&Save::parse(&s), &e, &z),
            _ => Err("bad input line".into()),
        };
        let reply = match got {
            Ok(n) => format!("{{\"coins\":{n}}}"),
            Err(e) => format!("{{\"error\":{:?}}}", e.replace('"', "'")),
        };
        let _ = writeln!(out, "{reply}");
    }
}
"##;

/// A save as the reference reads it.
struct SaveRef {
    level: i64,
    luck: i64,
    streak: i64,
    equipped: Vec<String>,
}

/// The reference: what the binary pays.
fn loot(w: &World, s: &SaveRef, enemy: &str, zone: &str) -> i64 {
    let e = w.enemies.iter().find(|e| e.id == enemy).expect("a known enemy");
    let z = w.zones.iter().find(|z| z.id == zone).expect("a known zone");
    let (mut greed, mut luck, mut seen) = (0, s.luck, HashSet::new());
    for id in &s.equipped {
        if !seen.insert(id) {
            continue;
        }
        let it = w.items.iter().find(|i| &i.id == id).expect("a known item");
        let sum = |code| it.effects.iter().filter(|(c, _)| *c == code).map(|(_, v)| v).sum::<i64>();
        greed += sum("GRD") * if it.slot == "trinket" { 2 } else { 1 };
        luck += sum("LCK");
    }
    let eff = if luck <= 10 { luck } else { 10 + (luck - 10) / 3 };
    let diff = s.level - e.level;
    let lvl = if diff > 4 {
        (100 - 15 * (diff - 4)).max(25)
    } else if diff < -3 {
        100 + 10 * (-diff - 3).min(5)
    } else {
        100
    };
    let mut v = e.bounty * TIER_PCT[z.tier as usize] / 100;
    v = v * lvl / 100;
    v = v * (100 + greed + 2 * eff) / 100;
    if e.class != "critter" {
        v += 3 * s.streak.min(8);
    }
    v.clamp(1, 999)
}

/// One unseen case: the save text, the enemy, the zone and the coins the game pays.
struct Case {
    save: String,
    enemy: String,
    zone: String,
    coins: i64,
}

/// The saves the agent never saw, with the coins the game pays for each.
fn cases(w: &World) -> Vec<Case> {
    let mut r = Rng::new(991);
    let ids: Vec<&str> = w.items.iter().map(|i| i.id.as_str()).collect();
    (0..CASES)
        .map(|k| {
            let n = r.int(0, 6) as usize;
            let mut eq: Vec<String> = r.sample(&ids, n).into_iter().map(String::from).collect();
            if !eq.is_empty() && eq.len() < 6 && r.chance(0.2) {
                eq.push(eq[0].clone());
            }
            let s = SaveRef { level: r.int(1, 50), luck: r.int(0, 25), streak: r.int(0, 15), equipped: eq };
            let save = format!(
                "name=Case{k}\nlevel={}\nluck={}\nstreak={}\nequipped={}\nbag={}\n",
                s.level,
                s.luck,
                s.streak,
                s.equipped.join(","),
                r.pick(&ids)
            );
            let enemy = r.pick(&w.enemies).id.clone();
            let zone = r.pick(&w.zones).id.clone();
            let coins = loot(w, &s, &enemy, &zone);
            Case { save, enemy, zone, coins }
        })
        .collect()
}

/// What in coins.rs could run a program, read a file or reach the game, by a plain scan of the
/// source with its line comments removed.
fn forbidden(src: &str) -> Vec<&'static str> {
    const BANNED: [&str; 13] = [
        "std::process",
        "process::",
        "Command",
        "std::fs",
        "fs::",
        "File",
        "std::net",
        "include_str",
        "include_bytes",
        "include!",
        "unsafe",
        "extern",
        "#[link",
    ];
    let code = bare_code(src);
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    BANNED
        .into_iter()
        .filter(|b| {
            code.match_indices(b).any(|(i, _)| {
                let before = code[..i].chars().next_back();
                let after = code[i + b.len()..].chars().next();
                // a word matches only as a whole token: `DataFile` and `external` are not `File` and `extern`
                !(ident(b.chars().next()) && ident(before)) && !(ident(b.chars().last()) && ident(after))
            })
        })
        .collect()
}

/// `src` with its comments removed and the text of its string and char literals blanked.
fn bare_code(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let at = |i: usize| c.get(i).copied();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match (c[i], at(i + 1)) {
            ('/', Some('/')) => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                let mut depth = 0;
                while i < c.len() {
                    match (c[i], at(i + 1)) {
                        ('/', Some('*')) => (depth, i) = (depth + 1, i + 2),
                        ('*', Some('/')) => (depth, i) = (depth - 1, i + 2),
                        _ => i += 1,
                    }
                    if depth == 0 {
                        break;
                    }
                }
                out.push(' ');
            }
            ('"', _) => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
                out.push_str("\"\"");
            }
            ('r', Some('"' | '#')) => {
                let hashes = c[i + 1..].iter().take_while(|&&h| h == '#').count();
                if at(i + 1 + hashes) != Some('"') {
                    out.push('r');
                    i += 1;
                    continue;
                }
                i += hashes + 2;
                while i < c.len()
                    && !(c[i] == '"' && c[i + 1..].iter().take(hashes).filter(|&&h| h == '#').count() == hashes)
                {
                    i += 1;
                }
                i += hashes + 1;
                out.push_str("\"\"");
            }
            ('\'', Some('\\')) => {
                i += 3;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
                out.push_str("' '");
            }
            ('\'', Some(_)) if at(i + 2) == Some('\'') => {
                i += 3;
                out.push_str("' '");
            }
            (ch, _) => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// The first C compiler found on PATH.
fn c_compiler() -> Option<&'static str> {
    ["gcc", "cc", "clang"].into_iter().find(|cc| {
        Command::new(cc)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Ok when this machine can build the game, compile answers and seal commands.
fn ready() -> Result<(), String> {
    c_compiler().ok_or("the realm task needs a C compiler (gcc, cc or clang)")?;
    seal::sysroot().ok_or("the realm task needs rustc")?;
    seal::available()
}

/// Compiles the game into `wd/realm`, stripped, from a source that never enters `wd`.
fn build(wd: &Path) -> Result<(), String> {
    let cc = c_compiler().ok_or("the realm task needs a C compiler (gcc, cc or clang)")?;
    let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let src = tmp.path().join("g.c");
    std::fs::write(&src, super::realm_game::source()).map_err(|e| e.to_string())?;
    let out = run(
        Command::new(cc).arg("-O2").arg("-s").arg("-o").arg(wd.join("realm")).arg(&src),
        None,
        Duration::from_secs(120),
    );
    if out.success() {
        Ok(())
    } else {
        Err(format!("the game did not compile: {}", out.tail(500)))
    }
}

/// The compiled answer's replies to the unseen cases, or why it could not run.
fn replies(wd: &Path, w: &World, cases: &[Case]) -> Result<Vec<String>, String> {
    let src = std::fs::read_to_string(wd.join("coins.rs")).map_err(|_| "coins.rs is missing".to_string())?;
    let bad = forbidden(&src);
    if !bad.is_empty() {
        return Err(format!("coins.rs must compute the coins itself, without programs or files: {bad:?}"));
    }
    let tmp = tempfile::Builder::new().prefix("nexo-bench-realm-").tempdir().map_err(|e| e.to_string())?;
    let t = tmp.path();
    std::fs::create_dir(t.join("data")).map_err(|e| e.to_string())?;
    for (rel, body) in data_files(w) {
        std::fs::write(t.join(rel), body).map_err(|e| e.to_string())?;
    }
    std::fs::write(t.join("harness.rs"), HARNESS).map_err(|e| e.to_string())?;
    std::fs::write(t.join("coins.rs"), src).map_err(|e| e.to_string())?;
    let input: String = cases
        .iter()
        .map(|c| serde_json::json!({"save": c.save, "enemy": c.enemy, "zone": c.zone}).to_string() + "\n")
        .collect();
    let exe = if cfg!(windows) { "check.exe" } else { "./check" };
    let line =
        format!("rustc -O --edition 2021 harness.rs -o check 2>build.log || {{ cat build.log >&2; exit 3; }}; {exe}");
    let mut cmd = match seal::Seal::new(t, vec![wd.to_path_buf()], vec![]) {
        Ok(s) => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(s.wrap(&line));
            c
        }
        Err(_) => {
            let build = run(
                Command::new("rustc").args(["-O", "--edition", "2021", "harness.rs", "-o", "check"]).current_dir(t),
                None,
                Duration::from_secs(240),
            );
            if !build.success() {
                return Err(format!("coins.rs does not compile: {}", build.tail(600)));
            }
            Command::new(t.join(exe))
        }
    };
    let out = run(cmd.current_dir(t).env_remove("RUSTC_WRAPPER"), Some(&input), Duration::from_secs(300));
    if out.code == Some(3) {
        return Err(format!("coins.rs does not compile: {}", out.tail(600)));
    }
    let got = answers(&out.stdout);
    if got.len() < cases.len() {
        return Err(format!("the harness stopped after {} of {} cases: {}", got.len(), cases.len(), out.tail(400)));
    }
    Ok(got)
}

/// The harness's replies in `stdout`, one per case, without what the answer printed itself.
fn answers(stdout: &str) -> Vec<String> {
    stdout.lines().filter(|l| l.starts_with("{\"coins\"") || l.starts_with("{\"error\"")).map(String::from).collect()
}

fn check(wd: &Path) -> Outcome {
    let w = world();
    let cs = cases(&w);
    let got = match replies(wd, &w, &cs) {
        Ok(g) => g,
        Err(why) => return Outcome::fail(why),
    };
    let mut wrong = Vec::new();
    for (c, line) in cs.iter().zip(&got) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
        if v["coins"].as_i64() != Some(c.coins) {
            wrong.push(format!("({}, {}, {}, {}, {line})", c.enemy, c.zone, c.save.replace('\n', " "), c.coins));
        }
    }
    let score = crate::check::round_to(1.0 - wrong.len() as f64 / cs.len() as f64, 4);
    let mut detail = format!("{:.1}% of {} unseen cases right", score * 100.0, cs.len());
    if let Some(first) = wrong.first() {
        detail += &format!("; e.g. (enemy, zone, save, expected, got) {first}");
    }
    Outcome { ok: wrong.is_empty(), score, detail }
}

/// The reference answer.
const SOLUTION: &str = r##"//! Coins per kill, as the game pays them.

use crate::{data, Save};
use std::collections::{HashMap, HashSet};

const TIER_PCT: [i64; 5] = [100, 120, 145, 175, 210];

fn rows(name: &str) -> HashMap<&'static str, Vec<&'static str>> {
    data(name).lines().filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| {
        let f: Vec<&str> = l.split('|').collect();
        (f[0], f)
    }).collect()
}

pub fn loot(save: &Save, enemy: &str, zone: &str) -> i64 {
    let (enemies, items, zones) = (rows("enemies.dat"), rows("items.dat"), rows("zones.dat"));
    let (e, z) = (&enemies[enemy], &zones[zone]);
    let (mut greed, mut luck, mut seen) = (0i64, save.luck, HashSet::new());
    for id in &save.equipped {
        if !seen.insert(id) {
            continue;
        }
        let it = &items[id.as_str()];
        for tok in it[3].split(';') {
            let (c, v) = tok.split_once('+').unwrap();
            let v: i64 = v.parse().unwrap();
            match c {
                "GRD" => greed += v * if it[1] == "trinket" { 2 } else { 1 },
                "LCK" => luck += v,
                _ => {}
            }
        }
    }
    let eff = if luck <= 10 { luck } else { 10 + (luck - 10) / 3 };
    let diff = save.level - e[3].parse::<i64>().unwrap();
    let lvl = if diff > 4 { (100 - 15 * (diff - 4)).max(25) } else if diff < -3 { 100 + 10 * (-diff - 3).min(5) } else { 100 };
    let mut v = e[6].parse::<i64>().unwrap() * TIER_PCT[z[1].parse::<usize>().unwrap()] / 100;
    v = v * lvl / 100;
    v = v * (100 + greed + 2 * eff) / 100;
    if e[2] != "critter" {
        v += 3 * save.streak.min(8);
    }
    v.clamp(1, 999)
}
"##;

fn solution(wd: &Path) {
    std::fs::write(wd.join("coins.rs"), SOLUTION).expect("a writable working directory");
}

const PROMPT: &str = "This folder holds a compiled game, ./realm. I need to know exactly how many coins a kill pays. \
                      Read RULES.md first: it says what you may and may not do, and what your final answer must be. \
                      Then work it out and write coins.rs.";

/// The realm task.
/// The box has no script interpreter: the answer is written in Rust and the system's tools are enough
/// to read the binary, and a compiled puzzle stays a puzzle about reading machine code.
const MASK: &[&str] = &["python", "python3", "pip", "pip3", "perl", "ruby", "node", "lua", "tclsh", "php"];

pub fn task() -> Task {
    let w = world();
    let mut files: Vec<(String, String)> = data_files(&w).into_iter().map(|(p, t)| (p.to_string(), t)).collect();
    let it = |k: usize| w.items[k].id.as_str();
    files.extend([
        ("RULES.md".to_string(), RULES.to_string()),
        ("MANUAL.txt".into(), MANUAL.into()),
        ("harness.rs".into(), HARNESS.into()),
        (
            "hero.sav".into(),
            format!("name=Ayla\nlevel=12\nluck=4\nstreak=2\nequipped={},{}\nbag={},{}\n", it(3), it(17), it(5), it(40)),
        ),
    ]);
    Task {
        id: "realm",
        trap: "reverse engineering a stripped binary: the rule is spread over the data, the binary and the save, \
               the manual is out of date, the data carry decoys, and the facts are found hundreds of steps apart",
        group: "hard",
        files,
        turns: vec![(1, PROMPT.to_string())],
        check,
        solution,
        setup: Some(build),
        sealed: true,
        mask: MASK,
        ready,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// `item -> effects` of the data.
    fn effects_by_item(w: &World) -> BTreeMap<&str, &[(&'static str, i64)]> {
        w.items.iter().map(|i| (i.id.as_str(), i.effects.as_slice())).collect()
    }

    #[test]
    fn the_data_keeps_its_sizes_and_decoys() {
        let w = world();
        assert_eq!((w.enemies.len(), w.items.len(), w.zones.len()), (180, 150, 16));
        assert!(effects_by_item(&w).values().any(|e| e.iter().any(|(c, _)| *c == "GLD")));
        assert!(w.enemies.iter().any(|e| e.class == "critter"));
        assert_eq!(cases(&w).len(), CASES);
    }

    #[test]
    fn coins_that_read_files_or_run_programs_are_rejected() {
        assert_eq!(forbidden("use std::process::Command;"), ["std::process", "process::", "Command"]);
        assert!(forbidden("// std::fs is not used\nfn loot() {}").is_empty());
        assert_eq!(forbidden("const G: &str = include_str!(\"realm\");"), ["include_str"]);
        assert_eq!(forbidden("let x = 1; std::fs::read(\"a\");"), ["std::fs", "fs::"]);
        assert_eq!(forbidden("let s = \"//\"; std::fs::read(\"a\");"), ["std::fs", "fs::"]);
        assert_eq!(forbidden("let c = '\"'; let r = r#\"x\"#; let t: File;"), ["File"]);
    }

    #[test]
    fn harmless_names_comments_and_strings_are_not_forbidden() {
        let src = "/* no std::fs here */\nenum DataFile { A }\nfn f<'a>(external_bonus: &'a str) -> &'a str {\n    \
                   let _ = \"Command\"; let _ = r#\"unsafe\"#; let _ = '\\''; external_bonus\n}\n";
        assert!(forbidden(src).is_empty(), "{:?}", forbidden(src));
    }

    #[test]
    fn lines_the_answer_prints_itself_are_not_replies() {
        let out = "greed=3\n{\"coins\":5}\ndebug\n{\"error\":\"x\"}\n";
        assert_eq!(answers(out), ["{\"coins\":5}", "{\"error\":\"x\"}"]);
    }

    #[test]
    fn the_realm_checker_gives_the_reference_solution_full_marks() {
        if let Err(why) = ready() {
            eprintln!("skipped: {why}");
            return;
        }
        let task = task();
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        assert!(wd.path().join("realm").exists());
        assert!(!(task.check)(wd.path()).ok, "no coins.rs yet");
        (task.solution)(wd.path());
        let got = (task.check)(wd.path());
        assert!(got.ok, "{}", got.detail);
        assert_eq!(got.score, 1.0);
    }

    #[test]
    fn the_game_pays_what_the_reference_says() {
        if let Err(why) = ready() {
            eprintln!("skipped: {why}");
            return;
        }
        let task = task();
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        let w = world();
        for c in cases(&w).iter().take(25) {
            std::fs::write(wd.path().join("case.sav"), &c.save).unwrap();
            let out = run(
                Command::new(wd.path().join("realm"))
                    .args(["duel", "--save", "case.sav", "--enemy", &c.enemy, "--zone", &c.zone])
                    .current_dir(wd.path()),
                None,
                Duration::from_secs(10),
            );
            assert!(out.stdout.contains(&format!("Loot: {} coins", c.coins)), "{}", out.tail(200));
        }
    }
}
