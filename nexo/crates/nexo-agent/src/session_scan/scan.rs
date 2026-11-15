//! Reading the directory of sessions with a cache of digests.

use super::model::*;
use super::parse::{day_of, digest_reader};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The digests kept between scans, per file and while the file stays as it was.
#[derive(Default)]
pub struct Cache {
    offset: i64,
    map: HashMap<PathBuf, Entry>,
}

pub(super) struct Entry {
    mtime: SystemTime,
    size: u64,
    digest: Arc<Digest>,
}

/// What a scan found.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub sessions: Vec<Arc<Digest>>,
    /// Sessions of the last [`KEEP_DAYS`] days, and how many of them were read (the rest are over the caps).
    pub eligible: usize,
    pub included: usize,
    /// Files parsed in this scan (the others came from the cache).
    pub parsed: usize,
    /// Today, as a day number.
    pub today: i64,
}

impl Report {
    /// The note for a scan that did not read everything.
    pub fn partial(&self) -> Option<String> {
        (self.included < self.eligible).then(|| format!("partial: {} of {} sessions", self.included, self.eligible))
    }
}

/// Scans `dir` (top level `*.jsonl` only) as of `now` (unix seconds) with days cut at `offset`
/// seconds east of UTC. A missing directory is an empty report.
pub fn scan(dir: &Path, now: i64, offset: i64, cache: &mut Cache) -> Result<Report, String> {
    if cache.offset != offset {
        cache.map.clear();
        cache.offset = offset;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Report { today: day_of(now, offset), ..Report::default() })
        }
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let oldest = now - KEEP_DAYS * 86_400;
    let mut files: Vec<(i64, SystemTime, u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        let Ok(mtime) = md.modified() else { continue };
        let secs = mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        if secs < oldest {
            continue;
        }
        files.push((secs, mtime, md.len(), path));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.3.cmp(&b.3)));
    let eligible = files.len();
    let mut report = Report { eligible, today: day_of(now, offset), ..Report::default() };
    let mut bytes = 0u64;
    let mut seen = Vec::new();
    for (_, mtime, size, path) in files {
        if report.included >= MAX_FILES {
            break;
        }
        if size > MAX_FILE {
            // left out for good: it counts among the sessions not read
            continue;
        }
        if bytes + size > MAX_BYTES {
            break;
        }
        bytes += size;
        let hit = cache.map.get(&path).filter(|e| e.mtime == mtime && e.size == size).map(|e| e.digest.clone());
        let digest = match hit {
            Some(d) => d,
            None => {
                let Ok(f) = std::fs::File::open(&path) else { continue };
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let d = Arc::new(digest_reader(&id, BufReader::with_capacity(64 * 1024, f), offset));
                report.parsed += 1;
                cache.map.insert(path.clone(), Entry { mtime, size, digest: d.clone() });
                d
            }
        };
        seen.push(path);
        report.included += 1;
        report.sessions.push(digest);
    }
    // files that are gone, or fell out of the window, are forgotten
    if cache.map.len() > seen.len() {
        let keep: std::collections::HashSet<&PathBuf> = seen.iter().collect();
        cache.map.retain(|p, _| keep.contains(p));
    }
    Ok(report)
}

/// [`scan`] of the sessions directory with the cache of this process.
pub fn scan_sessions(now: i64, offset: i64) -> Result<Report, String> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    scan(&nexo_core::paths::sessions_dir(), now, offset, &mut cache)
}

/// Now, in unix seconds, and the local offset east of UTC in seconds (0 where it cannot be told).
pub fn clock() -> (i64, i64) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() as i64;
    (now, local_offset())
}

pub(super) fn local_offset() -> i64 {
    if let Ok(o) = time::UtcOffset::current_local_offset() {
        return o.whole_seconds() as i64;
    }
    // a process with several threads cannot ask the system on some platforms: the shell can
    #[cfg(unix)]
    {
        if let Ok(out) = std::process::Command::new("date").arg("+%z").output() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1 } else { 1 };
                if let (Ok(h), Ok(m)) = (s[1..3].parse::<i64>(), s[3..5].parse::<i64>()) {
                    return sign * (h * 3600 + m * 60);
                }
            }
        }
    }
    0
}
