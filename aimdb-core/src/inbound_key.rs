//! Keys: small integers standing for the values of one capture, one table per
//! record.

use alloc::{sync::Arc, vec::Vec};
use core::num::NonZeroU16;
use core::sync::atomic::{AtomicU32, Ordering};

use hashbrown::HashMap;

#[cfg(feature = "std")]
type Mutex<T> = std::sync::Mutex<T>;
#[cfg(not(feature = "std"))]
type Mutex<T> = spin::Mutex<T>;

#[cfg(feature = "std")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(not(feature = "std"))]
fn lock<T>(m: &Mutex<T>) -> spin::MutexGuard<'_, T> {
    m.lock()
}

/// A capture value's key, assigned the first time the value is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(NonZeroU16);

impl KeyId {
    /// 0-based, for indexing per-key state.
    pub fn index(self) -> usize {
        usize::from(self.0.get()) - 1
    }
}

/// A record's key table. Grows as values arrive, up to `capacity`.
pub(crate) struct KeyTable {
    capacity: NonZeroU16,
    keys: Mutex<Keys>,
    dropped: AtomicU32,
}

#[derive(Default)]
struct Keys {
    by_name: HashMap<Arc<str>, KeyId>,
    names: Vec<Arc<str>>,
}

impl KeyTable {
    pub(crate) fn new(capacity: NonZeroU16) -> Self {
        Self {
            capacity,
            keys: Mutex::new(Keys::default()),
            dropped: AtomicU32::new(0),
        }
    }

    /// The key for `name`, assigned if new. `None` when the table is full;
    /// the caller drops the message and it is counted.
    pub(crate) fn key(&self, name: &str) -> Option<KeyId> {
        let mut keys = lock(&self.keys);
        if let Some(&id) = keys.by_name.get(name) {
            return Some(id);
        }
        let Some(id) = u16::try_from(keys.names.len() + 1)
            .ok()
            .filter(|&n| n <= self.capacity.get())
            .and_then(NonZeroU16::new)
            .map(KeyId)
        else {
            let _ = self
                .dropped
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1));
            return None;
        };
        let name: Arc<str> = name.into();
        keys.names.push(name.clone());
        keys.by_name.insert(name, id);
        Some(id)
    }

    /// The value `key` stands for.
    pub(crate) fn name(&self, key: KeyId) -> Option<Arc<str>> {
        lock(&self.keys).names.get(key.index()).cloned()
    }

    #[cfg(any(test, feature = "remote"))]
    pub(crate) fn capacity(&self) -> u16 {
        self.capacity.get()
    }

    #[cfg(any(test, feature = "remote"))]
    pub(crate) fn assigned(&self) -> usize {
        lock(&self.keys).names.len()
    }

    /// Messages turned away because the table was full.
    #[cfg(any(test, feature = "remote"))]
    pub(crate) fn dropped(&self) -> u32 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(capacity: u16) -> KeyTable {
        KeyTable::new(NonZeroU16::new(capacity).unwrap())
    }

    #[test]
    fn keys_are_assigned_in_order_and_stable() {
        let t = table(4);
        let a = t.key("kitchen").unwrap();
        let b = t.key("hall").unwrap();
        assert_eq!((a.index(), b.index()), (0, 1));
        assert_eq!(t.key("kitchen"), Some(a));
        assert_eq!(t.assigned(), 2);
        assert_eq!(t.name(a).as_deref(), Some("kitchen"));
        assert_eq!(t.name(b).as_deref(), Some("hall"));
    }

    #[test]
    fn full_table_drops_new_values_and_keeps_known_ones() {
        let t = table(2);
        let a = t.key("a").unwrap();
        t.key("b").unwrap();
        assert_eq!(t.key("c"), None);
        assert_eq!(t.key("d"), None);
        assert_eq!(t.key("a"), Some(a));
        assert_eq!((t.capacity(), t.assigned(), t.dropped()), (2, 2, 2));
    }

    #[test]
    fn unknown_key_has_no_name() {
        let big = table(8);
        let foreign = big.key("x").and(big.key("y")).unwrap();
        assert_eq!(table(8).name(foreign), None);
    }

    #[test]
    fn full_u16_capacity_does_not_overflow() {
        let t = table(u16::MAX);
        for i in 0..u16::MAX {
            assert_eq!(
                t.key(&alloc::format!("{i}")).map(KeyId::index),
                Some(usize::from(i))
            );
        }
        assert_eq!(t.key("one more"), None);
        assert_eq!(t.dropped(), 1);
    }
}
