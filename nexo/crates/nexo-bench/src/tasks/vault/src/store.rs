//! The documents. This layer only keeps them: who may see or change what is decided one level up,
//! in `service`.

use std::collections::BTreeMap;

use crate::users::UserId;

/// The id of a document; ids start at 1 and are never reused.
pub type DocId = u64;

/// One stored document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: DocId,
    pub owner: UserId,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub created: u64,
    pub modified: u64,
}

impl Document {
    /// The size of the body in bytes.
    pub fn size(&self) -> usize {
        self.body.len()
    }

    /// Whether the document carries this tag.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

/// All the documents, ordered by id.
#[derive(Debug)]
pub struct Store {
    docs: BTreeMap<DocId, Document>,
    next: DocId,
}

impl Default for Store {
    fn default() -> Self {
        Store::new()
    }
}

impl Store {
    /// An empty store.
    pub fn new() -> Store {
        Store { docs: BTreeMap::new(), next: 1 }
    }

    /// Keeps a new document and returns its id.
    pub fn insert(&mut self, owner: UserId, title: &str, body: &str, tags: Vec<String>, now: u64) -> DocId {
        let id = self.next;
        self.next += 1;
        let doc =
            Document { id, owner, title: title.to_string(), body: body.to_string(), tags, created: now, modified: now };
        self.docs.insert(id, doc);
        id
    }

    /// The document with this id.
    pub fn get(&self, id: DocId) -> Option<&Document> {
        self.docs.get(&id)
    }

    /// Replaces a body and returns the old size, or `None` when there is no such document.
    pub fn set_body(&mut self, id: DocId, body: &str, now: u64) -> Option<usize> {
        let doc = self.docs.get_mut(&id)?;
        let old = doc.body.len();
        doc.body = body.to_string();
        doc.modified = now;
        Some(old)
    }

    /// Takes a document out of the store.
    pub fn remove(&mut self, id: DocId) -> Option<Document> {
        self.docs.remove(&id)
    }

    /// How many documents there are.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// Whether there are no documents.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Every document, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Document> {
        self.docs.values()
    }

    /// The documents of one owner, in id order.
    pub fn by_owner(&self, owner: UserId) -> Vec<&Document> {
        self.iter().filter(|d| d.owner == owner).collect()
    }

    /// The bytes of body text one owner keeps.
    pub fn bytes_of(&self, owner: UserId) -> usize {
        self.by_owner(owner).iter().map(|d| d.size()).sum()
    }
}
