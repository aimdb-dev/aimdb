//! `Key<T>`: a record key bound to its contract (design 063).
//!
//! Lives here, not in `aimdb-core`, because `SchemaType` lives here and this
//! crate depends on core, not the other way round.

use core::marker::PhantomData;

use aimdb_core::{RecordKey, TypedKey, TypedName};

use crate::SchemaType;

/// A record key bound to the contract type `T` this build speaks.
pub struct Key<T> {
    name: &'static str,
    link: Option<&'static str>,
    _contract: PhantomData<fn() -> T>,
}

impl<T: SchemaType> Key<T> {
    /// A key for a name learned at run time. The name is interned (leaked):
    /// for registration at startup. A lookup by such a name borrows it: `aimdb_core::Name`.
    pub fn interned(name: &str) -> Self {
        Self::new(aimdb_core::StringKey::intern(name).as_str())
    }

    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            link: None,
            _contract: PhantomData,
        }
    }
    pub const fn with_link(self, address: &'static str) -> Self {
        Self {
            link: Some(address),
            ..self
        }
    }
}

// Manual impls: derives would require `T: Clone` etc.
impl<T> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Key<T> {}
impl<T> PartialEq for Key<T> {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl<T> Eq for Key<T> {}
impl<T> core::hash::Hash for Key<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state)
    }
}
impl<T> core::borrow::Borrow<str> for Key<T> {
    fn borrow(&self) -> &str {
        self.name
    }
}
impl<T> core::fmt::Debug for Key<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name)
    }
}
impl<T: 'static> RecordKey for Key<T> {
    fn as_str(&self) -> &'static str {
        self.name
    }
    fn link_address(&self) -> Option<&str> {
        self.link
    }
}
impl<T> TypedName for Key<T>
where
    T: SchemaType + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    type Contract = T;
    fn name(&self) -> &str {
        self.name
    }
}
impl<T> TypedKey for Key<T>
where
    T: SchemaType + Send + Sync + Clone + core::fmt::Debug + 'static,
{
    const SCHEMA: Option<(&'static str, u32)> = Some((T::NAME, T::VERSION));
}

impl<T> AsRef<str> for Key<T> {
    fn as_ref(&self) -> &str {
        self.name
    }
}
