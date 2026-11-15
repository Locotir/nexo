use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use tokio::sync::Mutex;

use super::server::Server;
use super::table::{marker_root, servers_for, usable};
use super::{FIRST, SETTLE};
/// The running servers, by command and root.
pub(super) type Running = Mutex<HashMap<(String, PathBuf), Arc<Server>>>;

pub(super) fn servers() -> &'static Running {
    static S: OnceLock<Running> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The server already running for `file`: same language, a root above the file (the deepest wins).
pub(super) async fn running_for(file: &Path, candidates: &[&[&str]]) -> Option<(Arc<Server>, PathBuf)> {
    let table = servers().lock().await;
    table
        .iter()
        .filter(|((cmd, root), s)| {
            file.starts_with(root)
                && !s.dead.load(Ordering::SeqCst)
                && candidates.iter().any(|c| cmd.starts_with(&c.join(" ")))
        })
        .max_by_key(|((_, root), _)| root.components().count())
        .map(|((_, root), s)| (s.clone(), root.clone()))
}

/// Servers being started for an edit, so one start per language and root.
pub(super) fn starting() -> &'static std::sync::Mutex<std::collections::HashSet<(String, PathBuf)>> {
    static S: OnceLock<std::sync::Mutex<std::collections::HashSet<(String, PathBuf)>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Starts, in the background, the server `file` would use if none runs yet, and gives it the file, so
/// the next edits find it warm. Never for Rust and Go (their compiler check answers) and never outside
/// a project; it does not wait and it fails silently.
pub fn warm(file: &Path) {
    let Some((language, candidates)) = servers_for(file) else { return };
    if matches!(language, "rust" | "go") {
        return;
    }
    let Some(cmd) = candidates.iter().find(|c| usable(c)) else { return };
    let Some(root) = marker_root(file) else { return };
    let env = crate::child_env().map(|e| format!(" {e:?}")).unwrap_or_default();
    let key = (format!("{}{env}", cmd.join(" ")), root.clone());
    let file = file.to_path_buf();
    tokio::spawn(async move {
        if servers().lock().await.contains_key(&key) {
            return;
        }
        let fresh = starting().lock().map(|mut s| s.insert(key.clone())).unwrap_or(false);
        if !fresh {
            return;
        }
        let started = Server::start(cmd, &root).await;
        if let Ok(server) = started {
            let server = Arc::new(server);
            servers().lock().await.entry(key.clone()).or_insert_with(|| server.clone());
            // opens the file, so the server begins to index and check it
            let _ = server.check_with(&file, language, FIRST, SETTLE).await;
        }
        if let Ok(mut s) = starting().lock() {
            s.remove(&key);
        }
    });
}
