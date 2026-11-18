use super::*;

/// Bytes kept of one line of output; the rest of a longer line is dropped as it comes.
pub const LINE_BYTES: usize = 16 * 1024;
/// How long output still in the pipes is read after the shell exits: a process it left running
/// may hold them open for good.
pub(super) const DRAIN: Duration = Duration::from_millis(300);

/// Sends each line `r` gives to `tx`, decoded lossily (a byte that is not UTF-8 does not stop the
/// reading) and cut to `LINE_BYTES` (a line with no end costs no more memory than that).
#[cfg(test)]
pub(super) async fn read_lines<R: tokio::io::AsyncRead + Unpin>(r: R, tx: mpsc::UnboundedSender<String>) {
    read_lines_logged(r, tx, None).await;
}

/// Maximum bytes retained per foreground command, shared by stdout and stderr.
pub(super) const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
pub(super) struct LogFile {
    pub file: std::fs::File,
    pub written: usize,
    pub truncated: bool,
}
pub(super) type RawLog = std::sync::Arc<std::sync::Mutex<Result<LogFile, String>>>;

/// Bytes a session may hold in its command logs: eight full logs.
#[cfg(unix)]
pub(super) const LOG_BUDGET_BYTES: u64 = 8 * MAX_LOG_BYTES as u64;

/// Logs being written right now, in every session of this process. An open log is empty on disk
/// and may still grow to `MAX_LOG_BYTES`, so each one is counted at that size; the mutex also
/// serialises the creation of logs, so two commands cannot both take the last slot.
#[cfg(unix)]
static LOGS_OPEN: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

/// The reservation of one open log, released when the command ends (its file is then counted at
/// its real size, or is gone).
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct LogSlot(std::path::PathBuf);

#[cfg(unix)]
impl Drop for LogSlot {
    fn drop(&mut self) {
        LOGS_OPEN.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != self.0);
    }
}

/// Creates the private log `path` in `dir` unless the folder's bytes plus the room reserved for the
/// logs being written leave less than one full log under `LOG_BUDGET_BYTES`.
#[cfg(unix)]
pub(super) fn open_log(dir: &std::path::Path, path: &std::path::Path) -> std::io::Result<(std::fs::File, LogSlot)> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut open = LOGS_OPEN.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(dir)?;
    let mut used = open.iter().filter(|p| p.parent() == Some(dir)).count() as u64 * MAX_LOG_BYTES as u64;
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        if !open.contains(&entry.path()) {
            used += entry.metadata().map_or(0, |m| m.len());
        }
    }
    if used + MAX_LOG_BYTES as u64 > LOG_BUDGET_BYTES {
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        return Err(std::io::Error::other(format!(
            "session log budget exhausted ({:.1} MiB kept or reserved of {:.0} MiB)",
            mib(used),
            mib(LOG_BUDGET_BYTES)
        )));
    }
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    open.push(path.to_path_buf());
    Ok((file, LogSlot(path.to_path_buf())))
}

pub(super) async fn read_lines_logged<R: tokio::io::AsyncRead + Unpin>(
    r: R,
    tx: mpsc::UnboundedSender<String>,
    log: Option<RawLog>,
) {
    let mut r = BufReader::new(r);
    let mut line: Vec<u8> = Vec::new();
    let mut cut = 0usize;
    loop {
        let (used, ended) = match r.fill_buf().await {
            Ok([]) | Err(_) => break,
            Ok(buf) => {
                let (chunk, ended) = match buf.iter().position(|b| *b == b'\n') {
                    Some(i) => (&buf[..i], true),
                    None => (buf, false),
                };
                let used = chunk.len() + usize::from(ended);
                if let Some(log) = &log {
                    use std::io::Write;
                    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(state) = &mut *log {
                        let keep = used.min(MAX_LOG_BYTES.saturating_sub(state.written));
                        if let Err(e) = state.file.write_all(&buf[..keep]) {
                            *log = Err(e.to_string());
                        } else {
                            state.written += keep;
                            state.truncated |= keep < used;
                        }
                    }
                }
                let room = LINE_BYTES.saturating_sub(line.len());
                line.extend_from_slice(&chunk[..chunk.len().min(room)]);
                cut += chunk.len().saturating_sub(room);
                (chunk.len() + usize::from(ended), ended)
            }
        };
        r.consume(used);
        if ended && tx.send(finish_line(&mut line, &mut cut)).is_err() {
            return;
        }
    }
    if !line.is_empty() || cut > 0 {
        let _ = tx.send(finish_line(&mut line, &mut cut));
    }
}

/// The line read so far as text, said to be cut when it was; both are reset.
pub(super) fn finish_line(line: &mut Vec<u8>, cut: &mut usize) -> String {
    if *cut > 0 {
        // a character split by the cut goes whole
        if let Err(e) = std::str::from_utf8(line) {
            if e.error_len().is_none() {
                *cut += line.len() - e.valid_up_to();
                line.truncate(e.valid_up_to());
            }
        }
    } else if line.last() == Some(&b'\r') {
        line.pop();
    }
    let mut s = String::from_utf8_lossy(line).into_owned();
    if *cut > 0 {
        s.push_str(&format!(" … [{} bytes of this line cut]", *cut));
    }
    line.clear();
    *cut = 0;
    s
}
