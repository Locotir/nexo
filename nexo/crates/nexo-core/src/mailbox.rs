//! The mailbox of one session: a folder of files other sessions write into and this one reads, so mail
//! survives a restart on either side and needs no daemon. Layout under `<data_dir>/mail/<session id>/`:
//! `session.json` (who this is and when it last beat), `inbox/`, `done/`, `bad/`, `receipts/` and
//! `sent.jsonl`. A mail is one file written whole and renamed into place, so a reader never sees half of it.
//!
//! Sending checks the limits (size, rate, repeats, reply chains, a full inbox); receiving applies the
//! inbound policy and parks accepted mail until the host says the session is idle. Nothing here reaches
//! the model by itself: [`Mailbox::tick`] hands the host the mails to deliver, and only when it said idle.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::agent::AgentEvent;
use crate::mail::*;
use crate::paths;
use crate::redact::redact;

/// The session file is rewritten this often (ms), and a session counts as live below `LIVE_MS` since the last beat.
pub const BEAT_MS: i64 = 5_000;
pub const LIVE_MS: i64 = 20_000;
/// Files read from the inbox per poll.
pub const POLL_MAX: usize = 8;
/// A held mail nobody decided on expires after this long.
pub const HOLD_MS: i64 = 5 * 60_000;
/// Delivered files and the mail of dead sessions are purged after this long.
pub const KEEP_MS: i64 = 24 * 3_600_000;
/// Sends per hour for one session, and the bucket per (sender, recipient): burst and refill per minute.
pub const HOURLY_MAX: usize = 20;
pub const BURST: f64 = 3.0;
pub const PER_MIN: f64 = 6.0;
/// The same text to the same session within this long is dropped.
pub const REPEAT_MS: i64 = 60_000;
/// Reply chains: the longest answered back and forth within `CHAIN_MS`.
pub const CHAIN_MAX: usize = 4;
pub const CHAIN_MS: i64 = 10 * 60_000;
const EVENTS_MAX: usize = 256;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Where the mailboxes live: `<data_dir>/mail`.
pub fn mail_root() -> PathBuf {
    paths::data_dir().join("mail")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionFile {
    v: u8,
    id: String,
    name: String,
    cwd: String,
    pid: u32,
    started: i64,
    beat: i64,
    bypass: bool,
}

/// One line of `sent.jsonl`: what a session wrote to another (the text is not kept, only its summary).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SentRec {
    pub id: String,
    pub from: String,
    #[serde(default)]
    pub from_name: String,
    pub to: String,
    #[serde(default)]
    pub to_name: String,
    pub summary: String,
    pub bytes: usize,
    pub at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return false;
        }
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        pid != 0
    }
}

fn make_dir(p: &Path) -> std::io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)
}

/// Writes `bytes` to `path` whole: a temporary file in the same folder, then a rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".tmp-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let r = o.open(&tmp).and_then(|mut f| f.write_all(bytes)).and_then(|_| fs::rename(&tmp, path));
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

fn age_ms(p: &Path, now: i64) -> i64 {
    let m = fs::metadata(p).and_then(|m| m.modified()).ok();
    let t = m.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64);
    t.map_or(0, |t| now - t)
}

fn text_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[derive(Default)]
struct Bucket {
    tokens: f64,
    at: i64,
    started: bool,
}

#[derive(Default)]
struct State {
    /// The session id this mailbox answers to (empty: not bound to a session yet).
    id: String,
    name: String,
    bypass: bool,
    inbound: Option<Inbound>,
    last_beat: i64,
    last_purge: i64,
    idle: bool,
    /// File names already read from the inbox.
    seen: HashSet<String>,
    ready: VecDeque<(Mail, PathBuf)>,
    held: Vec<(Mail, PathBuf, i64)>,
    /// Mails this process sent and the stage they last reached: id to (recipient, stage).
    sent: HashMap<String, (String, Stage)>,
    buckets: HashMap<String, Bucket>,
    hourly: VecDeque<i64>,
    repeats: HashMap<(String, u64), i64>,
    /// Mails seen in this process, sent or received: id to (reply_to, moment). For the reply chains.
    chain: HashMap<String, (Option<String>, i64)>,
    /// The last mail received from a session: id of that session to (mail id, moment).
    last_from: HashMap<String, (String, i64)>,
    events: Vec<AgentEvent>,
}

/// What one [`Mailbox::tick`] found for the host.
#[derive(Debug, Default)]
pub struct Tick {
    /// For the interface and the supervisor, as if the agent had sent them.
    pub events: Vec<AgentEvent>,
    /// Mails to hand over now (the session is idle).
    pub deliver: Vec<Mail>,
}

type Externals = Arc<dyn Fn() -> Vec<PeerInfo> + Send + Sync>;

pub struct Mailbox {
    root: PathBuf,
    cwd: String,
    started: i64,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    externals: Mutex<Option<Externals>>,
    st: Mutex<State>,
}

impl Mailbox {
    /// Opens (creating its folders) the mailbox of session `id` under `root`, and announces it.
    pub fn open(
        root: &Path,
        id: &str,
        name: &str,
        cwd: &str,
        bypass: bool,
        inbound: Inbound,
    ) -> std::io::Result<Arc<Mailbox>> {
        Mailbox::open_with_clock(root, id, name, cwd, bypass, inbound, Arc::new(now_ms))
    }

    /// As [`Mailbox::open`], with a clock of its own (tests move time by hand).
    pub fn open_with_clock(
        root: &Path,
        id: &str,
        name: &str,
        cwd: &str,
        bypass: bool,
        inbound: Inbound,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> std::io::Result<Arc<Mailbox>> {
        if !safe_id(id) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unusable session id"));
        }
        for d in ["inbox", "done", "bad", "receipts"] {
            make_dir(&root.join(id).join(d))?;
        }
        let now = clock();
        let mb = Mailbox {
            root: root.to_path_buf(),
            cwd: cwd.to_string(),
            started: now,
            clock,
            externals: Mutex::new(None),
            st: Mutex::new(State {
                id: id.to_string(),
                name: clean_name(name),
                bypass,
                inbound: Some(inbound),
                idle: false,
                ..Default::default()
            }),
        };
        mb.write_session(now);
        Ok(Arc::new(mb))
    }

    /// The id of the session this mailbox belongs to (empty until it is bound to one).
    pub fn id(&self) -> String {
        self.lock().id.clone()
    }

    /// A mailbox for a session that does not exist yet: nothing is written until [`Mailbox::rebind`] gives it one.
    pub fn unbound(root: &Path, cwd: &str, bypass: bool, inbound: Inbound) -> Arc<Mailbox> {
        Arc::new(Mailbox {
            root: root.to_path_buf(),
            cwd: cwd.to_string(),
            started: now_ms(),
            clock: Arc::new(now_ms),
            externals: Mutex::new(None),
            st: Mutex::new(State { bypass, inbound: Some(inbound), ..Default::default() }),
        })
    }

    /// The session changed (started, resumed, forked, cleared): from now on this is the mailbox of `id`.
    /// What waited for the old one stays in its folder, for when it is resumed.
    pub fn rebind(&self, id: &str, name: &str) {
        if !safe_id(id) {
            return;
        }
        let old = self.id();
        if old != id {
            if !old.is_empty() {
                let _ = fs::remove_file(self.root.join(&old).join("session.json"));
            }
            for d in ["inbox", "done", "bad", "receipts"] {
                let _ = make_dir(&self.root.join(id).join(d));
            }
            let mut s = self.lock();
            s.id = id.to_string();
            s.seen.clear();
            s.ready.clear();
            s.held.clear();
            s.sent.clear();
            s.last_from.clear();
        }
        self.lock().name = clean_name(name);
        self.write_session(self.now());
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn dir(&self) -> PathBuf {
        self.root.join(self.id())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// This session as the others see it.
    pub fn me(&self) -> PeerInfo {
        let s = self.lock();
        PeerInfo {
            id: s.id.clone(),
            name: s.name.clone(),
            cwd: self.cwd.clone(),
            pid: std::process::id(),
            external: false,
            bypass: s.bypass,
            sendable: true,
            me: true,
        }
    }

    pub fn set_name(&self, name: &str) {
        self.lock().name = clean_name(name);
        self.write_session(self.now());
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.lock().bypass = bypass;
        self.write_session(self.now());
    }

    /// The sessions of other programs the host can see (shown by `list_sessions`, never written to).
    pub fn set_externals(&self, f: Externals) {
        if let Ok(mut g) = self.externals.lock() {
            *g = Some(f);
        }
    }

    /// The host says whether the session can take a delivery now: true after `Idle`, false when a turn starts.
    pub fn set_idle(&self, idle: bool) {
        self.lock().idle = idle;
    }

    /// Rewrites the session file now (a session that is busy elsewhere keeps showing as live).
    pub fn beat(&self) {
        self.write_session(self.now());
    }

    fn write_session(&self, now: i64) {
        let (id, name, bypass) = {
            let mut s = self.lock();
            s.last_beat = now;
            (s.id.clone(), s.name.clone(), s.bypass)
        };
        if id.is_empty() {
            return;
        }
        let f = SessionFile {
            v: MAIL_VERSION,
            id,
            name,
            cwd: self.cwd.clone(),
            pid: std::process::id(),
            started: self.started,
            beat: now,
            bypass,
        };
        if let Ok(b) = serde_json::to_vec(&f) {
            let _ = write_atomic(&self.dir().join("session.json"), &b);
        }
    }

    /// Leaves: the session file goes, so the others stop seeing it.
    pub fn close(&self) {
        let _ = fs::remove_file(self.dir().join("session.json"));
    }

    /// The live sessions: this one (marked `me`), the others that beat in the last 20 s and whose process
    /// exists, then the external ones the host knows.
    pub fn peers(&self) -> Vec<PeerInfo> {
        let now = self.now();
        let me = self.me();
        let mut out = if me.id.is_empty() { Vec::new() } else { vec![me] };
        let mut rest = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.root) {
            for e in rd.flatten() {
                let sid = e.file_name().to_string_lossy().to_string();
                if sid == self.id() {
                    continue;
                }
                let Ok(text) = fs::read_to_string(e.path().join("session.json")) else { continue };
                let Ok(f) = serde_json::from_str::<SessionFile>(&text) else { continue };
                if f.id != sid || f.v != MAIL_VERSION || now - f.beat >= LIVE_MS || !pid_alive(f.pid) {
                    continue;
                }
                rest.push(PeerInfo {
                    id: f.id,
                    name: f.name,
                    cwd: f.cwd,
                    pid: f.pid,
                    external: false,
                    bypass: f.bypass,
                    sendable: true,
                    me: false,
                });
            }
        }
        rest.sort_by(|a, b| (a.name.as_str(), a.id.as_str()).cmp(&(b.name.as_str(), b.id.as_str())));
        out.extend(rest);
        let ext = self.externals.lock().ok().and_then(|g| g.clone());
        if let Some(f) = ext {
            for mut p in f() {
                p.external = true;
                p.sendable = false;
                p.me = false;
                out.push(p);
            }
        }
        out
    }

    /// The session `to` names: an exact id, an id prefix (four characters or more), a unique name, or
    /// `name [ref]`. An error says what to do instead.
    pub fn resolve(&self, to: &str) -> Result<PeerInfo, String> {
        let to = to.trim();
        let peers: Vec<PeerInfo> = self.peers().into_iter().filter(|p| !p.external).collect();
        let (name_part, ref_part) = match to.rfind('[') {
            Some(i) if to.ends_with(']') => (to[..i].trim(), Some(to[i + 1..to.len() - 1].trim())),
            _ => (to, None),
        };
        let pick = |c: Vec<&PeerInfo>| -> Result<PeerInfo, String> {
            match c.len() {
                1 => {
                    if c[0].me {
                        Err("Not sent: that is this session".to_string())
                    } else {
                        Ok(c[0].clone())
                    }
                }
                0 => Err(format!("Not sent: no live session \"{to}\"")),
                _ => {
                    let list: Vec<String> = c.iter().map(|p| describe(p)).collect();
                    Err(format!(
                        "Not sent: \"{to}\" fits several sessions: {}; name one as `name [ref]`",
                        list.join(", ")
                    ))
                }
            }
        };
        if let Some(p) = peers.iter().find(|p| p.id == to) {
            return pick(vec![p]);
        }
        if let Some(r) = ref_part {
            let c: Vec<&PeerInfo> = peers
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case(name_part) && r.len() >= 2 && p.id.starts_with(r))
                .collect();
            return pick(c);
        }
        if to.len() >= 4 {
            let c: Vec<&PeerInfo> = peers.iter().filter(|p| p.id.starts_with(to)).collect();
            if !c.is_empty() {
                return pick(c);
            }
        }
        let c: Vec<&PeerInfo> = peers.iter().filter(|p| p.name.eq_ignore_ascii_case(to)).collect();
        pick(c)
    }

    fn push_event(s: &mut State, ev: AgentEvent) {
        if s.events.len() >= EVENTS_MAX {
            s.events.remove(0);
        }
        s.events.push(ev);
    }

    /// The events queued since the last call (sends and arrivals).
    pub fn drain_events(&self) -> Vec<AgentEvent> {
        std::mem::take(&mut self.lock().events)
    }

    /// Writes `text` into the inbox of the session `to` names. `summary` None: the first line of the text.
    /// The error is written for the model to read.
    pub fn send(&self, to: &str, text: &str, summary: Option<&str>, kind: FromKind) -> Result<Mail, String> {
        let r = self.send_inner(to, text, summary, kind);
        if let Err(reason) = &r {
            let mut s = self.lock();
            Mailbox::push_event(&mut s, AgentEvent::MailFailed { to: to.to_string(), reason: reason.clone() });
        }
        r
    }

    fn send_inner(&self, to: &str, text: &str, summary: Option<&str>, kind: FromKind) -> Result<Mail, String> {
        if self.id().is_empty() {
            return Err("Not sent: this session is not ready yet".into());
        }
        if text.trim().is_empty() {
            return Err("Not sent: the message is empty".into());
        }
        if text.len() > MAIL_MAX_BYTES {
            return Err(format!("Not sent: too big ({} > {MAIL_MAX_BYTES} bytes)", text.len()));
        }
        let peer = self.resolve(to)?;
        let now = self.now();
        let inbox = self.root.join(&peer.id).join("inbox");
        let waiting = fs::read_dir(&inbox)
            .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
            .unwrap_or(0);
        if waiting >= INBOX_MAX {
            return Err(format!("Not sent: inbox of \"{}\" is full", peer.name));
        }
        let clean = redact(text).into_owned();
        // masking can make a text longer, and the receiver drops what is over the cap: judge what travels
        if clean.len() > MAIL_MAX_BYTES {
            return Err(format!(
                "Not sent: too big once secrets are masked ({} > {MAIL_MAX_BYTES} bytes)",
                clean.len()
            ));
        }
        let th = text_hash(&clean);
        let mut reply_to = None;
        {
            let mut s = self.lock();
            if kind == FromKind::Agent {
                // identical text to the same session within a minute
                s.repeats.retain(|_, t| now - *t < REPEAT_MS);
                if s.repeats.contains_key(&(peer.id.clone(), th)) {
                    return Err(format!("Not sent: the same message went to \"{}\" less than a minute ago", peer.name));
                }
                // 20 sends an hour
                while s.hourly.front().is_some_and(|t| now - *t >= 3_600_000) {
                    s.hourly.pop_front();
                }
                if s.hourly.len() >= HOURLY_MAX {
                    let wait = (s.hourly.front().map_or(0, |t| 3_600_000 - (now - *t)) + 999) / 1000;
                    return Err(format!(
                        "Not sent: {HOURLY_MAX} messages in the last hour, retry in {wait} s; batch your messages"
                    ));
                }
                // reply chains: back and forth with this session
                if let Some((last, at)) = s.last_from.get(&peer.id).cloned() {
                    if now - at < CHAIN_MS {
                        let mut depth = 1;
                        let mut cur = last.clone();
                        while let Some((Some(prev), at2)) = s.chain.get(&cur).cloned() {
                            if now - at2 >= CHAIN_MS || depth > CHAIN_MAX + 2 {
                                break;
                            }
                            depth += 1;
                            cur = prev;
                        }
                        if depth > CHAIN_MAX {
                            return Err(format!(
                                "Not sent: this is a long back and forth with \"{}\" ({depth} replies in a row); wait for the person",
                                peer.name
                            ));
                        }
                        reply_to = Some(last);
                    }
                }
                // the bucket for this recipient
                let b = s.buckets.entry(peer.id.clone()).or_default();
                if !b.started {
                    b.tokens = BURST;
                    b.started = true;
                } else {
                    b.tokens = (b.tokens + (now - b.at).max(0) as f64 / 60_000.0 * PER_MIN).min(BURST);
                }
                b.at = now;
                if b.tokens < 1.0 {
                    let wait = ((1.0 - b.tokens) / PER_MIN * 60.0).ceil() as i64;
                    return Err(format!("Not sent: rate limit, retry in {wait} s; batch your messages"));
                }
                b.tokens -= 1.0;
                s.hourly.push_back(now);
                s.repeats.insert((peer.id.clone(), th), now);
            }
        }
        let mail = Mail {
            v: MAIL_VERSION,
            id: new_id(now),
            from: self.me(),
            from_kind: kind,
            to: peer.id.clone(),
            summary: summary_of(&clean, summary),
            text: clean,
            at: now,
            reply_to,
        };
        let body = serde_json::to_vec(&mail).map_err(|e| format!("Not sent: {e}"))?;
        let path = inbox.join(format!("{now:013}-{}.json", mail.id));
        write_atomic(&path, &body).map_err(|e| format!("Not sent: {e}"))?;
        let rec = SentRec {
            id: mail.id.clone(),
            from: self.id(),
            from_name: mail.from.name.clone(),
            to: peer.id.clone(),
            to_name: peer.name.clone(),
            summary: mail.summary.clone(),
            bytes: mail.text.len(),
            at: now,
            reply_to: mail.reply_to.clone(),
        };
        self.append_ledger(&rec);
        let mut s = self.lock();
        s.sent.insert(mail.id.clone(), (peer.id.clone(), Stage::Sent));
        s.chain.insert(mail.id.clone(), (mail.reply_to.clone(), now));
        Mailbox::push_event(
            &mut s,
            AgentEvent::MailSent {
                id: mail.id.clone(),
                to: peer.id,
                to_name: peer.name,
                summary: mail.summary.clone(),
                bytes: mail.text.len(),
            },
        );
        Ok(mail)
    }

    fn append_ledger(&self, rec: &SentRec) {
        let Ok(mut line) = serde_json::to_string(rec) else { return };
        line.push('\n');
        let mut o = fs::OpenOptions::new();
        o.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        if let Ok(mut f) = o.open(self.dir().join("sent.jsonl")) {
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Tells the sender's side how far a mail got: an empty file `receipts/<mail id>.<stage>` in the sender's folder.
    pub fn receipt(&self, mail: &Mail, stage: Stage) {
        if !safe_id(&mail.from.id) || !safe_id(&mail.id) {
            return;
        }
        let d = self.root.join(&mail.from.id).join("receipts");
        if make_dir(&d).is_ok() {
            let _ = write_atomic(&d.join(format!("{}.{}", mail.id, stage.label())), b"");
        }
    }

    /// The loop accepted a delivery: receipts go out and the host hears `MailDelivered`.
    pub fn delivered(&self, mails: &[Mail]) -> AgentEvent {
        for m in mails {
            self.receipt(m, Stage::Delivered);
        }
        AgentEvent::MailDelivered { ids: mails.iter().map(|m| m.id.clone()).collect() }
    }

    /// The model began answering the turn that carried these mails.
    pub fn read(&self, mails: &[Mail]) -> AgentEvent {
        for m in mails {
            self.receipt(m, Stage::Read);
        }
        AgentEvent::MailRead { ids: mails.iter().map(|m| m.id.clone()).collect() }
    }

    /// Mails that wait for the person's approval.
    pub fn held(&self) -> Vec<Mail> {
        self.lock().held.iter().map(|(m, _, _)| m.clone()).collect()
    }

    /// Mails accepted and waiting for the session to be idle.
    pub fn waiting(&self) -> usize {
        self.lock().ready.len()
    }

    /// The person decides on a held mail (by id or id prefix): accepted ones join the queue, the others are refused.
    pub fn decide(&self, id: &str, accept: bool) -> bool {
        let mut s = self.lock();
        let Some(i) = s.held.iter().position(|(m, _, _)| m.id == id || (id.len() >= 4 && m.id.starts_with(id))) else {
            return false;
        };
        let (m, p, _) = s.held.remove(i);
        drop(s);
        if accept {
            self.lock().ready.push_back((m, p));
        } else {
            self.receipt(&m, Stage::Refused);
            self.archive(&p);
        }
        true
    }

    fn archive(&self, path: &Path) {
        if let Some(n) = path.file_name() {
            let _ = fs::rename(path, self.dir().join("done").join(n));
        }
    }

    /// One round of housekeeping, to run about once a second: beat, read the inbox (at most eight files),
    /// expire held mail, collect receipts for mail sent here, and, when the host said idle, take the mails to deliver.
    pub fn tick(&self) -> Tick {
        if self.id().is_empty() {
            return Tick::default();
        }
        let now = self.now();
        let (beat, purge) = {
            let s = self.lock();
            (now - s.last_beat >= BEAT_MS, now - s.last_purge >= 10 * 60_000)
        };
        if beat {
            self.write_session(now);
        }
        if purge {
            self.lock().last_purge = now;
            self.purge(now);
        }
        self.poll_inbox(now);
        self.expire(now);
        self.poll_receipts();
        let mut deliver = Vec::new();
        let dir = self.dir();
        {
            let mut s = self.lock();
            if s.idle && !s.ready.is_empty() {
                let mut bytes = 0;
                while let Some((m, _)) = s.ready.front() {
                    if deliver.len() >= DELIVER_MAX || (bytes > 0 && bytes + m.text.len() > DELIVER_BYTES) {
                        break;
                    }
                    bytes += m.text.len();
                    let Some((m, p)) = s.ready.pop_front() else { break };
                    // the file leaving the inbox is the claim: another process on the same session got it first
                    if !claim(&dir, &p) {
                        continue;
                    }
                    s.chain.insert(m.id.clone(), (m.reply_to.clone(), now));
                    s.last_from.insert(m.from.id.clone(), (m.id.clone(), now));
                    deliver.push(m);
                }
                if !deliver.is_empty() {
                    s.idle = false;
                }
            }
        }
        Tick { events: self.drain_events(), deliver }
    }

    fn poll_inbox(&self, now: i64) {
        let Ok(rd) = fs::read_dir(self.dir().join("inbox")) else { return };
        let mut names: Vec<(String, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                (n.ends_with(".json") && !n.starts_with('.')).then(|| (n, e.path()))
            })
            .collect();
        names.sort();
        let fresh: Vec<(String, PathBuf)> = {
            let s = self.lock();
            names.into_iter().filter(|(n, _)| !s.seen.contains(n)).take(POLL_MAX).collect()
        };
        for (name, path) in fresh {
            self.lock().seen.insert(name.clone());
            let mail = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Mail>(&t).ok()).filter(|m| {
                m.v == MAIL_VERSION
                    && m.to == self.id()
                    && safe_id(&m.id)
                    && safe_id(&m.from.id)
                    && m.text.len() <= MAIL_MAX_BYTES
            });
            let Some(mut mail) = mail else {
                let _ = fs::rename(&path, self.dir().join("bad").join(&name));
                continue;
            };
            mail.summary = summary_of(&mail.text, Some(&mail.summary));
            let (bypass, policy) = {
                let s = self.lock();
                (s.bypass, s.inbound.unwrap_or_default())
            };
            let verdict = policy.verdict(bypass, mail.from.bypass, mail.from_kind);
            let mut s = self.lock();
            s.chain.insert(mail.id.clone(), (mail.reply_to.clone(), now));
            match verdict {
                Verdict::Refuse => {
                    drop(s);
                    self.receipt(&mail, Stage::Refused);
                    self.archive(&path);
                }
                Verdict::Hold => {
                    Mailbox::push_event(
                        &mut s,
                        AgentEvent::MailArrived {
                            id: mail.id.clone(),
                            from: mail.from.id.clone(),
                            from_name: mail.from.name.clone(),
                            summary: mail.summary.clone(),
                            held: true,
                        },
                    );
                    s.held.push((mail.clone(), path, now));
                    drop(s);
                    self.receipt(&mail, Stage::Held);
                }
                Verdict::Accept => {
                    Mailbox::push_event(
                        &mut s,
                        AgentEvent::MailArrived {
                            id: mail.id.clone(),
                            from: mail.from.id.clone(),
                            from_name: mail.from.name.clone(),
                            summary: mail.summary.clone(),
                            held: false,
                        },
                    );
                    s.ready.push_back((mail, path));
                }
            }
        }
    }

    fn expire(&self, now: i64) {
        let gone: Vec<(Mail, PathBuf)> = {
            let mut s = self.lock();
            let mut keep = Vec::new();
            let mut gone = Vec::new();
            for (m, p, at) in std::mem::take(&mut s.held) {
                if now - at >= HOLD_MS {
                    gone.push((m, p));
                } else {
                    keep.push((m, p, at));
                }
            }
            s.held = keep;
            gone
        };
        for (m, p) in gone {
            self.receipt(&m, Stage::Expired);
            self.archive(&p);
        }
    }

    /// Reads the receipts of mail sent from here; every step forward becomes a `MailStage` event.
    fn poll_receipts(&self) {
        let Ok(rd) = fs::read_dir(self.dir().join("receipts")) else { return };
        let mut found: HashMap<String, Stage> = HashMap::new();
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let Some((id, st)) = n.rsplit_once('.') else { continue };
            let Some(stage) = Stage::parse(st) else { continue };
            let slot = found.entry(id.to_string()).or_insert(stage);
            if stage.rank() > slot.rank() {
                *slot = stage;
            }
        }
        let mut s = self.lock();
        let mut ev = Vec::new();
        for (id, stage) in found {
            if let Some((_, cur)) = s.sent.get_mut(&id) {
                if cur.advances_to(stage) {
                    *cur = stage;
                    ev.push(AgentEvent::MailStage { id, stage });
                }
            }
        }
        ev.sort_by_key(|e| match e {
            AgentEvent::MailStage { id, .. } => id.clone(),
            _ => String::new(),
        });
        for e in ev {
            Mailbox::push_event(&mut s, e);
        }
    }

    fn purge(&self, now: i64) {
        if let Ok(rd) = fs::read_dir(self.dir().join("done")) {
            for e in rd.flatten() {
                if age_ms(&e.path(), now) > KEEP_MS {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        if let Ok(rd) = fs::read_dir(self.dir().join("receipts")) {
            for e in rd.flatten() {
                if age_ms(&e.path(), now) > KEEP_MS {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
        let live: HashSet<String> = self.peers().into_iter().map(|p| p.id).collect();
        if let Ok(rd) = fs::read_dir(&self.root) {
            for e in rd.flatten() {
                let sid = e.file_name().to_string_lossy().to_string();
                if live.contains(&sid) {
                    continue;
                }
                if let Ok(files) = fs::read_dir(e.path().join("inbox")) {
                    for f in files.flatten() {
                        if age_ms(&f.path(), now) > KEEP_MS {
                            let _ = fs::remove_file(f.path());
                        }
                    }
                }
            }
        }
    }
}

/// Longest name a session goes by.
const NAME_MAX: usize = 48;

/// A session name that is safe to list and to put in a delivery: no secrets, no control characters, clipped.
fn clean_name(name: &str) -> String {
    let s: String = redact(name).chars().filter(|c| !c.is_control()).take(NAME_MAX).collect();
    s.trim().to_string()
}

/// An accepted mail leaves the inbox for `done/` when it is handed over. False when the file was already gone:
/// someone else (a second process on the same session) took it, and handing it over again would repeat it.
fn claim(dir: &Path, path: &Path) -> bool {
    let Some(n) = path.file_name() else { return true };
    match fs::rename(path, dir.join("done").join(n)) {
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        Ok(()) => true,
    }
}

/// `name [3fa9c1] cwd`: a session as an ambiguity lists it.
pub fn describe(p: &PeerInfo) -> String {
    let short: String = p.id.chars().take(6).collect();
    format!("{} [{short}] {}", p.name, p.cwd)
}

/// The ledgers: every `sent.jsonl` under `root` read from `offsets` on (bytes, per session id), with the new offsets.
pub fn read_ledgers(root: &Path, offsets: &mut HashMap<String, u64>) -> Vec<SentRec> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let sid = e.file_name().to_string_lossy().to_string();
        let Ok(data) = fs::read(e.path().join("sent.jsonl")) else { continue };
        let off = offsets.entry(sid).or_insert(0);
        if (*off as usize) > data.len() {
            *off = 0;
        }
        let tail = &data[*off as usize..];
        // only whole lines
        let end = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        for line in tail[..end].split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            if let Ok(r) = serde_json::from_slice::<SentRec>(line) {
                out.push(r);
            }
        }
        *off += end as u64;
    }
    out.sort_by_key(|r| r.at);
    out
}

/// The receipts that wait in session `sid`'s folder: `(mail id, stage)`, the furthest stage of each.
pub fn read_receipts(root: &Path, sid: &str) -> Vec<(String, Stage)> {
    let mut best: HashMap<String, Stage> = HashMap::new();
    if let Ok(rd) = fs::read_dir(root.join(sid).join("receipts")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            let Some((id, st)) = n.rsplit_once('.') else { continue };
            let Some(stage) = Stage::parse(st) else { continue };
            let slot = best.entry(id.to_string()).or_insert(stage);
            if stage.rank() > slot.rank() {
                *slot = stage;
            }
        }
    }
    let mut v: Vec<(String, Stage)> = best.into_iter().collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// The ids of the session folders under `root`.
pub fn session_ids(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bind_tests;
