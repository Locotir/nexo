use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::running::running_for;
use super::table::servers_for;
/// The most an `edit`/`write` waits for an already running server before it reports nothing. Unit
/// tests run hundreds of tests at once on a busy machine, where the fake server in them sometimes needs
/// more than 1.5 s to answer; there it is 4 s, still under the 5 s the slow fake sleeps, so the test of
/// a slow server keeps seeing the edit give up.
pub const QUICK: Duration = if cfg!(test) { Duration::from_secs(4) } else { Duration::from_millis(1500) };
/// After the first diagnostics of a quick check, how long to wait for more.
pub(super) const QUICK_SETTLE: Duration = Duration::from_millis(100);

/// Errors and warnings of `file` from a server that is already running, as `(line, text)` with errors
/// first. `None` when there is no such server or it does not answer within `bound`: an edit never waits
/// longer, and never starts a server.
pub async fn quick(file: &Path, bound: Duration) -> Option<Vec<(u64, String)>> {
    let (language, candidates) = servers_for(file)?;
    let work = async {
        let (server, root) = running_for(file, candidates).await?;
        let list = server.check_with(file, language, bound, QUICK_SETTLE).await.ok()?;
        Some((list, root))
    };
    let (list, root) = tokio::time::timeout(bound, work).await.ok()??;
    let shown = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
    let mut rows: Vec<(u64, u64, String)> = list
        .iter()
        .filter_map(|d| {
            let sev = d["severity"].as_u64().unwrap_or(1);
            if sev > 2 {
                return None;
            }
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let msg = d["message"].as_str().unwrap_or("").lines().next().unwrap_or("");
            let word = if sev == 1 { "error" } else { "warning" };
            let src = d["source"].as_str().map(|s| format!(" ({s})")).unwrap_or_default();
            Some((sev, line, format!("{shown}:{line}:{col} {word}: {msg}{src}")))
        })
        .collect();
    rows.sort();
    Some(rows.into_iter().map(|(_, line, text)| (line, text)).collect())
}
