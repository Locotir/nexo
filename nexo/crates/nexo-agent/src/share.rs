//! Shared memory: one file several agents read and write at the same moment.
//!
//! The file lives in the nexo folder (`~/.nexo/shared-memory.md`, or `NEXO_SHARED_MEMORY` in a test). Every
//! change is published by atomic replacement under a separate lock shared by all cooperating processes.
//! A line is `epoch | id | text`; readers can request entries newer than their last epoch.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use nexo_core::error::{ErrorKind, ProviderError as Error};

/// Lines one read returns at most.
pub const READ_CAP: usize = 200;
/// Characters of one line.
pub const LINE_CAP: usize = 2000;
/// Lines kept: past this the oldest go, under the same lock as a write.
const KEEP: usize = 4000;

/// One line of the shared memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The count of lines when this one was written: a reader that has seen up to `n` asks for `since: n`.
    pub epoch: u64,
    /// Who wrote it (a session id, or `person`).
    pub id: String,
    pub text: String,
}

/// Where the file is: `NEXO_SHARED_MEMORY` when set (a test), else `~/.nexo/shared-memory.md`.
pub fn path() -> Result<PathBuf, Error> {
    if let Some(p) = std::env::var_os("NEXO_SHARED_MEMORY") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| Error::new(ErrorKind::Other, "no home directory for the shared memory"))?;
    Ok(PathBuf::from(home).join(".nexo").join("shared-memory.md"))
}

/// The lines newer than `since` (0 reads them all), oldest first, up to [`READ_CAP`].
pub fn read_since(since: u64) -> Result<Vec<Entry>, Error> {
    let p = path()?;
    let guard = store_lock(&p, false)?;
    let out = (|| {
        let mut f = match File::open(&p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        };
        let entries = read_locked(&mut f, 0)?;
        if let Some(first) = entries.first().filter(|entry| entry.epoch > since.saturating_add(1)) {
            return Err(Error::new(ErrorKind::Other, format!(
                "shared memory: requested entries are no longer retained; use since {} to read the oldest available entry",
                first.epoch - 1
            )));
        }
        Ok(entries.into_iter().filter(|entry| entry.epoch > since).take(READ_CAP).collect())
    })();
    let _ = unlock(&guard);
    out
}

/// Appends one line and returns it with the epoch it took. Two calls at once both land, in either order.
pub fn append(id: &str, text: &str) -> Result<Entry, Error> {
    let text = one_line(text);
    if text.is_empty() {
        return Err(Error::new(ErrorKind::Other, "nothing to write: the line is empty"));
    }
    if text.chars().count() > LINE_CAP {
        return Err(Error::new(ErrorKind::Other, format!("shared memory: text exceeds {LINE_CAP} characters")));
    }
    let p = path()?;
    let guard = store_lock(&p, true)?;
    let out = (|| {
        let all = match File::open(&p) {
            Ok(mut f) => read_locked(&mut f, 0)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io(e)),
        };
        let epoch = all
            .last()
            .map(|e| e.epoch)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::Other, "shared memory: epoch overflow"))?;
        let entry = Entry { epoch, id: clean_id(id), text };
        let start = all.len().saturating_sub(KEEP - 1);
        rewrite(&p, &all[start..], &entry)?;
        Ok(entry)
    })();
    let _ = unlock(&guard);
    out
}

/// The lines as one block for a prompt: each `epoch | id | text`, or an empty string when there are none.
pub fn block(entries: &[Entry]) -> String {
    entries.iter().map(|e| format!("{} | {} | {}", e.epoch, e.id, e.text)).collect::<Vec<_>>().join("\n")
}

fn read_locked(f: &mut File, since: u64) -> Result<Vec<Entry>, Error> {
    f.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut out = Vec::new();
    let mut previous: u64 = 0;
    let mut reader = BufReader::new(f.try_clone().map_err(io)?);
    let mut index = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(io)? == 0 {
            break;
        }
        index += 1;
        if !line.ends_with('\n') {
            return Err(Error::new(ErrorKind::Other, format!("shared memory: incomplete line {index}")));
        }
        line.pop();
        let e =
            parse(&line).ok_or_else(|| Error::new(ErrorKind::Other, format!("shared memory: invalid line {index}")))?;
        if e.epoch == 0 || (previous != 0 && previous.checked_add(1) != Some(e.epoch)) {
            return Err(Error::new(ErrorKind::Other, format!("shared memory: invalid epoch on line {index}")));
        }
        previous = e.epoch;
        if e.epoch > since {
            out.push(e);
        }
    }
    Ok(out)
}

// Every update is published by atomic replacement while holding a separate, stable lock.
// An interrupted write can leave a temporary file, but never a partial published record.
fn rewrite(p: &Path, keep: &[Entry], add: &Entry) -> Result<(), Error> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = p.as_os_str().to_os_string();
    name.push(format!(".tmp-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let temp = PathBuf::from(name);
    let result = (|| {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&temp).map_err(io)?;
        if let Ok(metadata) = std::fs::metadata(p) {
            f.set_permissions(metadata.permissions()).map_err(io)?;
        }
        for e in keep.iter().chain(std::iter::once(add)) {
            writeln!(f, "{} | {} | {}", e.epoch, e.id, e.text).map_err(io)?;
        }
        f.sync_all().map_err(io)?;
        drop(f);
        std::fs::rename(&temp, p).map_err(io)?;
        // Persist the directory entry as well as the file contents on Unix.
        #[cfg(unix)]
        File::open(parent_dir(p)).and_then(|dir| dir.sync_all()).map_err(io)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn parent_dir(p: &Path) -> &Path {
    p.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
}

fn store_lock(p: &Path, exclusive: bool) -> Result<File, Error> {
    std::fs::create_dir_all(parent_dir(p)).map_err(io)?;
    let mut name = p.as_os_str().to_os_string();
    name.push(".lock");
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let guard = opts.open(PathBuf::from(name)).map_err(io)?;
    lock(&guard, exclusive)?;
    Ok(guard)
}

fn parse(line: &str) -> Option<Entry> {
    let (epoch, rest) = line.split_once('|')?;
    let (id, text) = rest.split_once('|')?;
    Some(Entry { epoch: epoch.trim().parse().ok()?, id: id.trim().to_string(), text: text.trim().to_string() })
}

/// Normalize to one line: a newline inside the text would be a second entry.
fn one_line(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat
}

fn clean_id(id: &str) -> String {
    let id: String = id.chars().filter(|c| !c.is_whitespace() && *c != '|').take(40).collect();
    if id.is_empty() {
        "anon".into()
    } else {
        id
    }
}

fn io(e: std::io::Error) -> Error {
    Error::new(ErrorKind::Other, format!("shared memory: {e}"))
}

fn lock(f: &File, exclusive: bool) -> Result<(), Error> {
    if exclusive { f.lock() } else { f.lock_shared() }.map_err(io)
}

fn unlock(f: &File) -> Result<(), Error> {
    f.unlock().map_err(io)
}

/// True when `child` is the same file as `root` or lives inside it.
pub fn inside(root: &Path, child: &Path) -> bool {
    child.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated(f: impl FnOnce()) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("shared.md");
        // a lock so the tests of one binary do not share one file through the environment
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NEXO_SHARED_MEMORY", &p);
        f();
        std::env::remove_var("NEXO_SHARED_MEMORY");
    }

    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    #[test]
    fn new_store_is_private() {
        use std::os::unix::fs::PermissionsExt;
        isolated(|| {
            append("person", "note").unwrap();
            assert_eq!(std::fs::metadata(path().unwrap()).unwrap().permissions().mode() & 0o077, 0);
        });
    }

    #[test]
    fn a_line_is_read_back_with_its_epoch() {
        isolated(|| {
            assert!(read_since(0).unwrap().is_empty());
            let a = append("s1", "first").unwrap();
            assert_eq!(a.epoch, 1);
            let b = append("s2", "second line").unwrap();
            assert_eq!(b.epoch, 2);
            assert_eq!(read_since(0).unwrap().len(), 2);
            // a reader that has seen the first asks for what is newer
            let newer = read_since(1).unwrap();
            assert_eq!(newer.len(), 1);
            assert_eq!(newer[0].text, "second line");
            assert_eq!(newer[0].id, "s2");
        });
    }

    #[test]
    fn a_newline_inside_the_text_is_one_line() {
        isolated(|| {
            append("s", "one\ntwo | three").unwrap();
            let all = read_since(0).unwrap();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].text, "one two | three");
        });
    }

    #[test]
    fn an_empty_line_is_refused() {
        isolated(|| {
            assert!(append("s", "   \n  ").is_err());
            assert!(read_since(0).unwrap().is_empty());
        });
    }

    #[test]
    fn two_writers_at_once_both_keep_their_line() {
        isolated(|| {
            let p = path().unwrap();
            let n = 32;
            std::thread::scope(|scope| {
                for i in 0..n {
                    scope.spawn(move || {
                        append(&format!("w{i}"), &format!("line {i}")).unwrap();
                    });
                }
            });
            let all = read_since(0).unwrap();
            assert_eq!(all.len(), n, "a line was lost");
            let mut epochs: Vec<u64> = all.iter().map(|e| e.epoch).collect();
            epochs.sort();
            assert_eq!(epochs, (1..=n as u64).collect::<Vec<_>>(), "two lines took the same epoch");
            let mut texts: Vec<String> = all.iter().map(|e| e.text.clone()).collect();
            texts.sort();
            let mut want: Vec<String> = (0..n).map(|i| format!("line {i}")).collect();
            want.sort();
            assert_eq!(texts, want);
            // the file itself holds every line, nothing half written
            let raw = std::fs::read_to_string(&p).unwrap();
            assert_eq!(raw.lines().count(), n);
        });
    }
    #[test]
    fn incremental_reads_return_oldest_first_without_skips() {
        isolated(|| {
            for i in 0..(READ_CAP + 37) {
                append("writer", &i.to_string()).unwrap();
            }
            let first = read_since(0).unwrap();
            assert_eq!(first.len(), READ_CAP);
            assert_eq!(first.first().unwrap().epoch, 1);
            let second = read_since(first.last().unwrap().epoch).unwrap();
            assert_eq!(second.len(), 37);
            assert_eq!(second.last().unwrap().epoch, (READ_CAP + 37) as u64);
        });
    }

    #[test]
    fn retention_keeps_latest_entries_and_epochs() {
        isolated(|| {
            // Seed efficiently; the append itself must enforce retention.
            let body: String = (1..=KEEP).map(|i| format!("{i} | seed | line {i}\n")).collect();
            std::fs::write(path().unwrap(), body).unwrap();
            assert_eq!(append("writer", "new").unwrap().epoch, (KEEP + 1) as u64);
            let raw = std::fs::read_to_string(path().unwrap()).unwrap();
            assert_eq!(raw.lines().count(), KEEP);
            assert!(raw.starts_with("2 | seed |"));
            assert!(read_since(0).is_err());
            assert_eq!(read_since(KEEP as u64).unwrap()[0].text, "new");
        });
    }

    #[test]
    fn malformed_and_unreadable_lines_are_errors() {
        isolated(|| {
            std::fs::write(path().unwrap(), b"1 | a | ok\nnot an entry\n").unwrap();
            assert!(read_since(0).is_err());
            assert!(append("a", "new").is_err());
            std::fs::write(path().unwrap(), b"1 | a | \xff\n").unwrap();
            assert!(read_since(0).is_err());
        });
    }

    #[test]
    fn rejects_incomplete_gaps_and_oversized_notes() {
        isolated(|| {
            let p = path().unwrap();
            std::fs::write(&p, b"1 | a | partial").unwrap();
            assert!(read_since(0).is_err());
            assert!(append("a", "next").is_err());
            std::fs::write(&p, b"1 | a | first\n3 | a | third\n").unwrap();
            assert!(read_since(0).is_err());
            assert!(append("a", "next").is_err());
            std::fs::remove_file(&p).unwrap();
            assert!(append("a", &"x".repeat(LINE_CAP + 1)).is_err());
            assert!(read_since(0).unwrap().is_empty());
        });
    }

    #[test]
    fn process_worker() {
        if std::env::var_os("NEXO_SHARE_WORKER").is_none() {
            return;
        }
        for i in 0..20 {
            append("process", &format!("note {i}")).unwrap();
            read_since(0).unwrap();
        }
    }

    #[test]
    fn independent_processes_share_stable_lock_across_replacements() {
        isolated(|| {
            let exe = std::env::current_exe().unwrap();
            let mut children: Vec<_> = (0..4)
                .map(|_| {
                    std::process::Command::new(&exe)
                        .arg("--exact")
                        .arg("share::tests::process_worker")
                        .env("NEXO_SHARE_WORKER", "1")
                        .spawn()
                        .unwrap()
                })
                .collect();
            for child in &mut children {
                assert!(child.wait().unwrap().success());
            }
            let all = read_since(0).unwrap();
            assert_eq!(all.len(), 80);
            for (i, entry) in all.iter().enumerate() {
                assert_eq!(entry.epoch, i as u64 + 1);
            }
        });
    }

    #[test]
    fn readers_and_writers_overlap_without_partial_lines() {
        isolated(|| {
            use std::sync::{Arc, Barrier};
            let barrier = Arc::new(Barrier::new(5));
            std::thread::scope(|scope| {
                for writer in 0..4 {
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        for i in 0..50 {
                            append(&format!("w{writer}"), &format!("line {i}")).unwrap();
                        }
                    });
                }
                barrier.wait();
                let mut epoch = 0;
                while epoch < 200 {
                    for entry in read_since(epoch).unwrap() {
                        assert_eq!(entry.epoch, epoch + 1);
                        assert!(entry.text.starts_with("line "));
                        epoch = entry.epoch;
                    }
                    std::thread::yield_now();
                }
            });
        });
    }
}
