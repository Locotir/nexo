//! The rules: who may create, read, change and delete which document, on top of the storage.
//!
//! A document belongs to its owner. Admins may do everything with every document; guests may not
//! write at all.

use crate::audit::AuditLog;
use crate::error::{Result, VaultError};
use crate::query;
use crate::quota::Quota;
use crate::store::{DocId, Document, Store};
use crate::users::{Directory, Role, UserId};

/// The longest title, in characters.
pub const MAX_TITLE: usize = 120;

/// The largest body, in bytes.
pub const MAX_BODY: usize = 64 * 1024;

/// The whole application state.
#[derive(Debug)]
pub struct Service {
    pub users: Directory,
    pub store: Store,
    pub quota: Quota,
    pub audit: AuditLog,
    clock: u64,
}

impl Service {
    /// An empty service where each user may keep `quota_bytes` of body text.
    pub fn new(quota_bytes: usize) -> Service {
        Service {
            users: Directory::new(),
            store: Store::new(),
            quota: Quota::new(quota_bytes),
            audit: AuditLog::new(500),
            clock: 0,
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Adds an account.
    pub fn register(&mut self, name: &str, role: Role, password: &str) -> Result<UserId> {
        let id = self.users.add(name, role, password)?;
        self.audit.record(id, "register", &id.to_string());
        Ok(id)
    }

    fn require_writer(&self, user: UserId) -> Result<()> {
        match self.users.role_of(user) {
            Some(role) if role.can_write() => Ok(()),
            Some(_) => Err(VaultError::Denied("guests cannot write".into())),
            None => Err(VaultError::Unauthenticated(format!("no user {user}"))),
        }
    }

    /// Whether `user` may see and change `doc`: the owner and the admins may.
    pub fn can_access(&self, user: UserId, doc: &Document) -> bool {
        doc.owner == user || self.users.is_admin(user)
    }

    fn not_found(id: DocId) -> VaultError {
        VaultError::NotFound(format!("document {id}"))
    }

    /// Keeps a new document for `user`.
    pub fn create(&mut self, user: UserId, title: &str, body: &str, tags: Vec<String>) -> Result<DocId> {
        self.require_writer(user)?;
        let title = title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE {
            return Err(VaultError::Invalid(format!("a title has 1 to {MAX_TITLE} characters")));
        }
        if body.len() > MAX_BODY {
            return Err(VaultError::Invalid(format!("a body has at most {MAX_BODY} bytes")));
        }
        self.quota.charge(user, body.len())?;
        let now = self.tick();
        let id = self.store.insert(user, title, body, tags, now);
        self.audit.record(user, "create", &id.to_string());
        Ok(id)
    }

    /// A document `user` may read.
    pub fn fetch(&self, user: UserId, id: DocId) -> Result<&Document> {
        let doc = self.store.get(id).ok_or_else(|| Self::not_found(id))?;
        if !self.can_access(user, doc) {
            return Err(VaultError::Denied(format!("document {id} belongs to someone else")));
        }
        Ok(doc)
    }

    /// Replaces the body of a document `user` may change.
    pub fn update(&mut self, user: UserId, id: DocId, body: &str) -> Result<()> {
        self.require_writer(user)?;
        let doc = self.store.get(id).ok_or_else(|| Self::not_found(id))?;
        if !self.can_access(user, doc) {
            return Err(VaultError::Denied(format!("document {id} belongs to someone else")));
        }
        if body.len() > MAX_BODY {
            return Err(VaultError::Invalid(format!("a body has at most {MAX_BODY} bytes")));
        }
        let owner = doc.owner;
        let old = doc.size();
        if body.len() >= old {
            self.quota.charge(owner, body.len() - old)?;
        } else {
            self.quota.release(owner, old - body.len());
        }
        let now = self.tick();
        self.store.set_body(id, body, now);
        self.audit.record(user, "update", &id.to_string());
        Ok(())
    }

    /// Removes a document and gives its bytes back to the owner's quota.
    pub fn delete(&mut self, user: UserId, id: DocId) -> Result<Document> {
        self.require_writer(user)?;
        let doc = self.store.remove(id).ok_or_else(|| Self::not_found(id))?;
        self.quota.release(doc.owner, doc.size());
        self.audit.record(user, "delete", &id.to_string());
        Ok(doc)
    }

    /// The documents `user` owns; an admin gets every document.
    pub fn list(&self, user: UserId) -> Vec<&Document> {
        if self.users.is_admin(user) {
            self.store.iter().collect()
        } else {
            self.store.by_owner(user)
        }
    }

    /// The documents of `user` whose title contains `term`.
    pub fn search(&self, user: UserId, term: &str) -> Result<Vec<&Document>> {
        let ids = query::search(&self.store, user, term)?;
        Ok(ids.into_iter().filter_map(|id| self.store.get(id)).collect())
    }
}
