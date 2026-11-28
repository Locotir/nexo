//! The settings of a curator: the default spec and the parsing of `key=value` specs into engine parameters.

use indexmap::IndexMap;

use crate::engine::Params;

/// The settings every spec starts from (what is given replaces them). The view is pure (no directives in the
/// system prompt, no requests for notes): an answered request keeps only its reply, outputs nothing drew on go
/// idle, repeated lines and repeated calls go once, and what the agent has not yet written about, or read of a
/// file the request names, stays. Measured on recorded checkpoints of the golden session: as many answers as
/// the whole history (+0.68 of 13) at 62 % of what it sends. A cut (idle steps, unused outputs, the cap)
/// is made only when what it saves is 2.25 times what rewriting the view behind it costs (`margin=2.25`): a cut
/// that guesses the future wrong pays the rewrite and, if the step comes back, its price without the cache.
/// While the thread works through the files its first message lists, what was read of one no request has
/// taken up yet stays (`mission=1`): the task named it, and cutting it would only bring it back without
/// the cache. An output an earlier call gave, line for line, goes as one line that points to it (and says
/// nothing changed when the same call gave it; `asked=3`): it is decided as it arrives, so it breaks no cache, and the copy above stays whole.
/// Every step stays in the history, a call made over and over too: a view of the same shape as
/// one sent before (call ids and the counts in curator lines aside) is never sent again, since at a fixed
/// temperature the model takes the same step again. Folding such a run kept the agent in it for 22 steps
/// on the golden session. The output the agent has just asked for comes whole (`fresh=1`); its repeated runs go
/// as pointers from the next request on, so no pointer sends it looking for the lines. An output the agent
/// has not written about yet (no text and no note since) leaves only as an exact copy of a later one, never
/// on a guess, for its latest 32000 tokens (`still=32000`): a new request does not mean the agent is done with
/// what it was reading. Without it, the golden session lost the reads of a request left unanswered as the
/// next one came in, and the agent read the same file again for 180 steps. Past the budget a guess may take
/// the oldest, so a session where the agent never writes still has a bounded view.
/// The working window: the view aims at `target=10000` tokens (the priced cap above runs with that budget). A
/// rewrite is a cost decision, not a threshold (`cap=0`): the view is cut down to the target, in one rewrite (the
/// oldest results that are not sticky leave first), once keeping its extra tokens for the calls still to come
/// costs more than sending the new prefix again without the cache (`Engine::fire_point`; the cache price is the
/// provider's, when the host gave it). Between rewrites the view only grows, so its prefix stays cached. The hard
/// ceiling is the model's own window, 95 % of it (the host gives it with `Race::window`): past it the oldest
/// sticky results leave too, as pointers. Only with no window known, `hard` (0: none) is the ceiling, and a `cap`
/// above 0 a fixed trigger. Between two rewrites (an epoch, `epoch=1`) nothing before the tail changes: no topic split, no
/// closing of answered requests, no cut; all of that waits for the next epoch. Sticky, by rule and with no
/// model: the request, the agent's own text and calls, the latest `file now` of each path, the last `check:`,
/// the latest whole read of each path, what a `recall` brought back until the step after it, and what the agent
/// has read and not yet written about (the latest `still` tokens of it, whatever the first message takes).
pub const D_SPEC: &str =
    "threshold=0.02;head=turn;recall=1;link=0.5;rule=rent;birth=squeeze;eoq=16000;ledger=6000;notes=3000;rules=2000;pin=4000;protect=12;directives=0;nudge=0;close=2;ahead=0.25;margin=2.25;dedup=3;live=1;lead=1;aim=1;follow=1;brief=1;digest=1;hold=1;asked=3;fresh=1;still=32000;sup=1;named=64000;mission=1;apart=8000;check=4000;every=4;ratio=0.1;least=0.0004;floor=0.6;cap=0;target=10000;hard=0;cut=age;cards=8;stale=1;pockets=0;epoch=1;life=3600;kept=0;lcalls=12;lahead=0.2;lmargin=1;trim=0;need=0;relations=1;segments=0";

pub(crate) const INTS: &[&str] = &[
    "protect",
    "check",
    "cap",
    "target",
    "hard",
    "every",
    "cards",
    "eoq",
    "ledger",
    "gwin",
    "jit",
    "notes",
    "rules",
    "pin",
    "nudge",
    "directives",
    "recall",
    "settle",
    "dedup",
    "close",
    "sup",
    "topic",
    "apart",
    "noted",
    "named",
    "mission",
    "sticky",
    "live",
    "lead",
    "aim",
    "follow",
    "brief",
    "digest",
    "hold",
    "still",
    "loops",
    "asked",
    "fresh",
    "recite",
    "task",
    "reruns",
    "stall",
    "stale",
    "pockets",
    "epoch",
    "kept",
    "trim",
    "need",
    "relations",
    "segments",
];
pub(crate) const STRS: &[&str] = &["rule", "birth", "stub", "head", "leave", "spill", "outcome", "cut"];
pub(crate) const FLOATS: &[&str] = &[
    "threshold",
    "ratio",
    "margin",
    "floor",
    "least",
    "link",
    "early",
    "trust",
    "ttl",
    "lerr",
    "ahead",
    "life",
    "lcalls",
    "lahead",
    "lmargin",
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    I(i64),
    F(f64),
    S(String),
}

/// The `key=value` pairs of a spec, in order. Only `;` separates them: a value may hold a comma.
pub(crate) fn parse_params(spec: &str) -> Result<IndexMap<String, String>, String> {
    let mut out = IndexMap::new();
    for part in spec.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once('=') else {
            return Err(format!("{part:?} is not key=value"));
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(out)
}

/// A value of the type its key takes; a number is never negative and always finite.
pub(crate) fn typed(k: &str, v: &str) -> Result<Val, String> {
    if STRS.contains(&k) {
        return Ok(Val::S(v.to_string()));
    }
    let digits = v.replace('_', "");
    if INTS.contains(&k) {
        match digits.parse::<i64>() {
            Ok(x) if x >= 0 => Ok(Val::I(x)),
            _ => Err(format!("{k} takes a whole number of 0 or more, not {v:?}")),
        }
    } else {
        match digits.parse::<f64>() {
            Ok(x) if x.is_finite() && x >= 0.0 => Ok(Val::F(x)),
            _ => Err(format!("{k} takes a number of 0 or more, not {v:?}")),
        }
    }
}

/// Settings kept so older specs still read, with the one value the built-in curator runs.
pub(crate) const ONLY: &[(&str, &str)] = &[
    ("rule", "rent"),
    ("stub", "plain"),
    ("leave", "oldest"),
    ("spill", ""),
    ("early", "0"),
    ("settle", "0"),
    ("topic", "0"),
    ("noted", "0"),
    ("sticky", "0"),
    ("loops", "0"),
    ("recite", "0"),
    ("task", "0"),
    ("reruns", "0"),
    ("stall", "0"),
    ("trust", "1"),
    ("ttl", "0"),
];

/// The engine a spec describes, or why it cannot run here.
pub(crate) fn params(spec: &str) -> Result<Params, String> {
    let given = parse_params(spec)?;
    if given.contains_key("model") {
        return Err("model is not supported by the built-in curator".into());
    }
    let mut kv = parse_params(D_SPEC)?;
    kv.extend(given);
    let mut vals: IndexMap<String, Val> = IndexMap::new();
    for (k, v) in &kv {
        if !INTS.contains(&k.as_str()) && !STRS.contains(&k.as_str()) && !FLOATS.contains(&k.as_str()) {
            return Err(format!("unknown setting {k:?}"));
        }
        vals.insert(k.clone(), typed(k, v)?);
    }
    let unsupported = |k: &str| Err(format!("{k} is not supported by the built-in curator"));
    for k in ["outcome", "gwin", "lerr", "jit"] {
        if vals.contains_key(k) {
            return unsupported(k);
        }
    }
    for (k, only) in ONLY {
        if vals.get(*k).is_some_and(|v| *v != typed(k, only).expect("a value it takes")) {
            return unsupported(k);
        }
    }
    // every value has its default in D_SPEC
    let missing = |k: &str| format!("{k} has no default");
    let f = |k: &str| match vals.get(k) {
        Some(Val::F(x)) => Ok(*x),
        _ => Err(missing(k)),
    };
    let i = |k: &str| match vals.get(k) {
        Some(Val::I(x)) => Ok(*x),
        _ => Err(missing(k)),
    };
    let s = |k: &str| match vals.get(k) {
        Some(Val::S(x)) => Ok(x.clone()),
        _ => Err(missing(k)),
    };
    let cut = s("cut")?;
    if !["age", "need", "copies", "cover"].contains(&cut.as_str()) {
        return unsupported("cut");
    }
    let head = s("head")?;
    if head != "turn" && head != "keep" {
        return unsupported("head");
    }
    let (floor, eoq) = (f("floor")?, i("eoq")?);
    if i("target")? > 0 && floor == 0.0 && eoq == 0 {
        return Err("target needs a floor above 0".into());
    }
    let (target, cap, hard) = (i("target")?, i("cap")?, i("hard")?);
    if cap > 0 && target > cap {
        return Err("target is above cap".into());
    }
    if hard > 0 && cap > hard {
        return Err("cap is above hard".into());
    }
    let birth: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for p in s("birth")?.replace(',', "+").split('+').map(str::trim).filter(|p| !p.is_empty()) {
            // a stage the curator does not know would take the place of the default and run nothing
            if !["squeeze"].contains(&p) {
                return Err(format!("birth has no stage {p:?}"));
            }
            if !v.iter().any(|x| x == p) {
                v.push(p.to_string());
            }
        }
        v
    };
    let relations = match i("relations")? {
        0 => false,
        1 => true,
        _ => return Err("relations takes 0 or 1".into()),
    };
    let segments = match i("segments")? {
        0 => false,
        1 => true,
        _ => return Err("segments takes 0 or 1".into()),
    };
    if segments && !relations {
        return Err("segments requires relations=1".into());
    }
    Ok(Params {
        relations,
        segments,
        threshold: f("threshold")?,
        protect: i("protect")?,
        check: i("check")?,
        ratio: f("ratio")?,
        margin: f("margin")?,
        floor,
        every: i("every")?,
        least: f("least")?,
        target,
        cap,
        hard,
        turn_head: head == "turn",
        eoq,
        ledger: i("ledger")?,
        link: f("link")?,
        cards: i("cards")?,
        birth,
        notes: i("notes")?,
        rules: i("rules")?,
        pin: i("pin")?,
        nudge: i("nudge")?,
        directives: i("directives")?,
        recall: i("recall")?,
        dedup: i("dedup")?,
        cut_need: cut == "need",
        cut_copies: cut == "copies",
        cut_cover: cut == "cover",
        close: i("close")?,
        ahead: f("ahead")?,
        sup: i("sup")?,
        apart: i("apart")?,
        named: i("named")?,
        mission: i("mission")?,
        live: i("live")?,
        lead: i("lead")?,
        aim: i("aim")?,
        follow: i("follow")?,
        brief: i("brief")?,
        digest: i("digest")?,
        hold: i("hold")?,
        still: i("still")?,
        asked: i("asked")?,
        fresh: i("fresh")?,
        stale: i("stale")?,
        pockets: i("pockets")?,
        epoch: i("epoch")?,
        life: f("life")?,
        kept: i("kept")?,
        lcalls: f("lcalls")?,
        lahead: f("lahead")?,
        lmargin: f("lmargin")?,
        trim: i("trim")?,
        need: i("need")?,
    })
}
