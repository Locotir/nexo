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
