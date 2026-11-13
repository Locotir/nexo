use super::*;

/// Parameters of one cold scale session.
#[derive(Debug, Clone)]
pub struct ColdParams {
    /// Distinct topics the session works through before the questions (each holds three facts).
    pub topics: usize,
    /// Facts a question correlates, each from another topic (1, 2 or 3; 0: questions take 1, 2, 3 in turn).
    pub hops: usize,
    pub seed: u64,
    /// Questions asked after the topics, each after a few switches to fresh topics.
    pub questions: usize,
    /// Every this many topics the person pastes a big text (log, numeric table or prose).
    pub paste_every: usize,
    pub paste_lines: usize,
    /// The curator is called on every `stride`-th request (1: every request, as the host does) and always on questions.
    pub stride: usize,
    /// Stop the session when it has run this long (seconds) or its process holds this much memory (MB); 0: no limit.
    pub max_secs: f64,
    pub max_rss_mb: f64,
    /// Standing facts planted early in the session (0 to 10), each asked about by 3 tasks that suppose its default.
    pub standing: usize,
    /// The model's window in tokens for the curator arm and the sierra arm (0: the curator is unlimited and the sierra
    /// arm runs at `SAW_WINDOW`). The plain arm is always the unlimited reference.
    pub window: u64,
}

impl ColdParams {
    pub fn new(topics: usize, hops: usize, seed: u64) -> ColdParams {
        ColdParams {
            topics,
            hops,
            seed,
            questions: 30,
            paste_every: 10,
            paste_lines: 80,
            stride: 1,
            max_secs: 0.0,
            max_rss_mb: 0.0,
            standing: 0,
            window: 0,
        }
    }

    /// A session of about `tokens` tokens: as many topics as that takes, measured on the first 200 (the text of
    /// a topic depends on the seed and its position only).
    pub fn for_tokens(tokens: u64, hops: usize, seed: u64) -> ColdParams {
        use nexo_curator::sim::price::tokens_of;
        let mut p = ColdParams::new(200, hops, seed);
        let g = Cold::new(p.clone());
        let per: u64 = (0..200).flat_map(|t| g.topic(t)).map(|m| tokens_of(&m) as u64).sum::<u64>() / 200;
        p.topics = (tokens / per.max(1)).max(200) as usize;
        p
    }
}

pub(super) fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub(super) fn mix(seed: u64, a: u64, b: u64) -> u64 {
    splitmix(seed ^ splitmix(a ^ splitmix(b)))
}

const OWNERS: [&str; 16] = [
    "alder",
    "birch",
    "cedar",
    "dunmore",
    "elmwood",
    "fenwick",
    "gable",
    "harlow",
    "ironside",
    "juniper",
    "kestrel",
    "lindqvist",
    "marlow",
    "norwood",
    "oakley",
    "pemberton",
];

/// The names of the three facts of a topic, in order.
pub const FACTS: [&str; 3] = ["port", "version", "owner"];

/// The three source lines of topic `t` (a port, a version and an owner), a function of the seed alone.
pub fn cold_facts(seed: u64, t: usize) -> [String; 3] {
    let h = mix(seed, t as u64, 1);
    [
        format!("[topic {t}] port = {}", 1024 + h % 60000),
        format!("[topic {t}] version = {}.{}.{}", (h >> 20) % 9 + 1, (h >> 28) % 40, (h >> 36) % 100),
        format!("[topic {t}] owner = {}{:03}", OWNERS[((h >> 44) % 16) as usize], (h >> 50) % 1000),
    ]
}

/// A step of the session: messages to append (alternating roles, the first a user message unless noted) and,
/// for a question block, the source lines its first message needs in the view; for a task block, the source
/// line of the standing fact whose default the task supposes (`stand`).
pub struct Block {
    pub msgs: Vec<Value>,
    pub ask: Option<Vec<String>>,
    pub stand: Option<Vec<String>>,
}

/// Ways a standing fact reaches the history: the person says it, the agent notes it (`Finding:`), or a
/// `cat` of a config file shows it.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Via {
    User,
    Model,
    Cat,
}

/// A standing fact that overrides a default, as planted: how each source words it, and the task that supposes
/// the default without naming the fact.
pub struct Fixed {
    pub(super) via: Via,
    /// The source line the oracle looks for in the view (a `key = value` line of a config, a sentence of the
    /// person, or what follows `Finding:` in the agent's reply).
    pub line: String,
    /// The request that supposes the default.
    pub task: &'static str,
}

/// Standing facts a session can plant, at most.
pub const FIXED_MAX: usize = 10;
/// Tasks that suppose the default of each planted fact.
pub const STAND_REPS: usize = 3;

/// Fact `i` of a session of `seed`: wording a function of the seed and the index alone. The shell rc, the
/// database port, the token file, the legacy folder, the search tool, the staging host, the editor config,
/// the logs, the build output and the backups: each overrides what a model would assume.
pub fn fixed_fact(seed: u64, i: usize) -> Fixed {
    let v = 100 + mix(seed, i as u64, 7) % 900;
    // (how it comes, the line, the task)
    let (via, line, task) = match i % FIXED_MAX {
        0 => (Via::Cat, format!("shell_rc = /etc/skel-{v}/rc"), "Add an alias to my shell."),
        1 => (
            Via::Model,
            format!("the database (postgres) listens on port {}, not 5432", 6000 + v),
            "Connect to the database and count the rows in users.",
        ),
        2 => (Via::Cat, format!("api_token_file = /run/secrets/api{v}"), "Rotate the API token."),
        3 => {
            (Via::User, format!("Don't touch /srv/legacy{v}, it is the legacy folder."), "Clean up the legacy folder.")
        }
        4 => (Via::Model, "searching the repo goes with rg, not grep".to_string(), "Search the repo for TODO."),
        5 => (
            Via::User,
            format!("The staging server is at stage{v}.internal:8797, not localhost:8080."),
            "Deploy to staging.",
        ),
        6 => {
            (Via::Cat, format!("editor_config = /home/dev/.config/edit{v}/init.vim"), "Change a setting in my editor.")
        }
        7 => (
            Via::User,
            format!("The app logs are stored in /data/logs{v}, not /var/log/app."),
            "Look through the app logs for errors.",
        ),
        8 => (
            Via::Model,
            format!("build artifacts are kept in /data/out{v}, not ./dist"),
            "Copy the build artifacts to the release folder.",
        ),
        _ => (
            Via::User,
            format!("Backups are stored in /mnt/backup{v}, not /tmp."),
            "Run a backup of the database dump.",
        ),
    };
    Fixed { via, line, task }
}
