//! Shared setup for the integration tests: a scratch folder and a handler with a few users.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use vault::files::FileRoot;
use vault::handler::Handler;
use vault::service::Service;
use vault::users::Role;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A folder under the system temp dir that is removed when it goes out of scope.
pub struct Scratch {
    pub dir: PathBuf,
}

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("vault-test-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch { dir }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A service with an admin `root`, members `alice` and `bob`, and a guest `gary`.
pub fn service() -> Service {
    let mut s = Service::new(10_000);
    s.register("root", Role::Admin, "root-secret1").unwrap();
    s.register("alice", Role::Member, "alice-secret1").unwrap();
    s.register("bob", Role::Member, "bob-secret1").unwrap();
    s.register("gary", Role::Guest, "gary-secret1").unwrap();
    s
}

/// A handler over `service()` whose attachments live in `scratch`.
pub fn handler(scratch: &Scratch) -> Handler {
    Handler::new(service(), "test-secret", FileRoot::new(scratch.dir.join("files")))
}

/// The token of one of the users made by `service()`.
pub fn login(h: &mut Handler, name: &str) -> String {
    let r = h.handle("", &format!("LOGIN {name} {name}-secret1"));
    assert_eq!(r.status, 200, "{r:?}");
    r.body
}
