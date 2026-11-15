//! Who is live: listing peers, resolving a name or id to one, and describing them.

use std::fs;

use super::*;

/// Longest name a session goes by.
pub(super) const NAME_MAX: usize = 48;

/// A session name that is safe to list and to put in a delivery: no secrets, no control characters, clipped.
pub(super) fn clean_name(name: &str) -> String {
    clean(name, NAME_MAX)
}

/// Text of another session that is safe to list: no secrets, no control characters, at most `max` characters.
pub(super) fn clean(text: &str, max: usize) -> String {
    let s: String = redact(text).chars().filter(|c| !c.is_control()).take(max).collect();
    s.trim().to_string()
}

/// The session file of `sid` when its heartbeat is fresh and its process exists: the test of being live.
pub(super) fn read_live(root: &Path, sid: &str, now: i64) -> Option<SessionFile> {
    let text = fs::read_to_string(root.join(sid).join("session.json")).ok()?;
    let f = serde_json::from_str::<SessionFile>(&text).ok()?;
    (f.id == sid && f.v == MAIL_VERSION && now - f.beat < LIVE_MS && pid_alive(f.pid)).then_some(f)
}

/// `name [3fa9c1] cwd`: a session as an ambiguity lists it.
pub fn describe(p: &PeerInfo) -> String {
    let short: String = p.id.chars().take(6).collect();
    format!("{} [{short}] {}", p.name, p.cwd)
}

/// The other live sessions a message can reach, as a short line for an error to end with.
pub(super) fn reachable(peers: &[PeerInfo]) -> String {
    let others: Vec<&PeerInfo> = peers.iter().filter(|p| !p.me).collect();
    if others.is_empty() {
        return "No other session is live.".into();
    }
    let names: Vec<String> = others
        .iter()
        .take(8)
        .map(|p| {
            if others.iter().filter(|q| q.name.eq_ignore_ascii_case(&p.name)).count() > 1 {
                format!("{} [{}]", p.name, p.id.chars().take(6).collect::<String>())
            } else {
                p.name.clone()
            }
        })
        .collect();
    let more = if others.len() > 8 { format!(" and {} more", others.len() - 8) } else { String::new() };
    format!("Live sessions: {}{more}.", names.join(", "))
}

impl Mailbox {
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
                let Some(f) = read_live(&self.root, &sid, now) else { continue };
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
                        Err(format!(
                            "Not sent: \"{to}\" is this session, the message would go to itself. {}",
                            reachable(&peers)
                        ))
                    } else {
                        Ok(c[0].clone())
                    }
                }
                0 => Err(format!("Not sent: no live session \"{to}\". {}", reachable(&peers))),
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
}
