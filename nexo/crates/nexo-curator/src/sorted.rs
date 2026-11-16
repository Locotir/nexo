//! The hash containers the engine keeps and saves (`snapshot.rs`): the same maps and sets as `rustc-hash`'s, which
//! they dereference to, but written in the order of their keys. A hash container goes through its table in an
//! order that depends on how it grew, so one read back from a file could write out other bytes than the one that
//! was saved, and two saves of the same state could differ. Sorted, a state saved, read and saved again is the
//! same bytes, which the tests of `engine/snapshot.rs` compare.

use std::hash::Hash;
use std::ops::{Deref, DerefMut};

use rustc_hash::FxBuildHasher;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

type Build = FxBuildHasher;

/// A `rustc_hash::FxHashMap` that is written in the order of its keys.
#[derive(Clone, Debug)]
pub struct FxHashMap<K, V>(rustc_hash::FxHashMap<K, V>);

/// A `rustc_hash::FxHashSet` that is written in sorted order.
#[derive(Clone, Debug)]
pub struct FxHashSet<T>(rustc_hash::FxHashSet<T>);

impl<K: Eq + Hash, V: PartialEq> PartialEq for FxHashMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: Eq + Hash> PartialEq for FxHashSet<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<K, V> From<rustc_hash::FxHashMap<K, V>> for FxHashMap<K, V> {
    fn from(m: rustc_hash::FxHashMap<K, V>) -> Self {
        FxHashMap(m)
    }
}

impl<T> From<rustc_hash::FxHashSet<T>> for FxHashSet<T> {
    fn from(m: rustc_hash::FxHashSet<T>) -> Self {
        FxHashSet(m)
    }
}

impl<K, V> Default for FxHashMap<K, V> {
    fn default() -> Self {
        FxHashMap(Default::default())
    }
}

impl<T> Default for FxHashSet<T> {
    fn default() -> Self {
        FxHashSet(Default::default())
    }
}

impl<K, V> Deref for FxHashMap<K, V> {
    type Target = rustc_hash::FxHashMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> DerefMut for FxHashMap<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Deref for FxHashSet<T> {
    type Target = rustc_hash::FxHashSet<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for FxHashSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<K: Eq + Hash, V> FxHashMap<K, V> {
    pub fn with_capacity_and_hasher(n: usize, _: Build) -> Self {
        FxHashMap(rustc_hash::FxHashMap::with_capacity_and_hasher(n, Build::default()))
    }
}

impl<T: Eq + Hash> FxHashSet<T> {
    pub fn with_capacity_and_hasher(n: usize, _: Build) -> Self {
        FxHashSet(rustc_hash::FxHashSet::with_capacity_and_hasher(n, Build::default()))
    }
}

impl<K: Eq + Hash, V> FromIterator<(K, V)> for FxHashMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(it: I) -> Self {
        FxHashMap(it.into_iter().collect())
    }
}

impl<T: Eq + Hash> FromIterator<T> for FxHashSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(it: I) -> Self {
        FxHashSet(it.into_iter().collect())
    }
}

impl<K: Eq + Hash, V> Extend<(K, V)> for FxHashMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, it: I) {
        self.0.extend(it);
    }
}

impl<T: Eq + Hash> Extend<T> for FxHashSet<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, it: I) {
        self.0.extend(it);
    }
}

impl<'a, T: Eq + Hash + Copy + 'a> Extend<&'a T> for FxHashSet<T> {
    fn extend<I: IntoIterator<Item = &'a T>>(&mut self, it: I) {
        self.0.extend(it.into_iter().copied());
    }
}

impl<K, V> IntoIterator for FxHashMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::hash_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a FxHashMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::hash_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut FxHashMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = std::collections::hash_map::IterMut<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<T> IntoIterator for FxHashSet<T> {
    type Item = T;
    type IntoIter = std::collections::hash_set::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a FxHashSet<T> {
    type Item = &'a T;
    type IntoIter = std::collections::hash_set::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<K: Serialize + Ord, V: Serialize> Serialize for FxHashMap<K, V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&K, &V)> = self.0.iter().collect();
        entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        let mut map = s.serialize_map(Some(entries.len()))?;
        for (k, v) in entries {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de, K: Deserialize<'de> + Eq + Hash, V: Deserialize<'de>> Deserialize<'de> for FxHashMap<K, V> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        rustc_hash::FxHashMap::deserialize(d).map(FxHashMap)
    }
}

impl<T: Serialize + Ord> Serialize for FxHashSet<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut items: Vec<&T> = self.0.iter().collect();
        items.sort_unstable();
        let mut seq = s.serialize_seq(Some(items.len()))?;
        for x in items {
            seq.serialize_element(x)?;
        }
        seq.end()
    }
}

impl<'de, T: Deserialize<'de> + Eq + Hash> Deserialize<'de> for FxHashSet<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        rustc_hash::FxHashSet::deserialize(d).map(FxHashSet)
    }
}
