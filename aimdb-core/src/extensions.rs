//! Generic extension storage for [`AimDbBuilder`](crate::AimDbBuilder) and [`AimDb`](crate::AimDb).
//!
//! External crates store typed state here during builder configuration
//! and retrieve it during record setup or at query time. This is the
//! hook mechanism used by `aimdb-persistence` — and any other crate that
//! needs to attach data to the builder or the live database without
//! modifying `aimdb-core`.
//!
//! # Example
//! ```no_run
//! # use aimdb_core::{AimDb, AimDbBuilder, RecordRegistrar};
//! # struct MyState { flag: bool }
//! # fn at_configure_time(builder: &mut AimDbBuilder) {
//! // Storing a value (e.g. from an external "with_persistence" builder ext):
//! builder.extensions_mut().insert(MyState { flag: true });
//! # }
//! # fn in_registrar(reg: &mut RecordRegistrar<'_, u32>) {
//! // Retrieving it from a RecordRegistrar closure:
//! let state = reg.extensions().get::<MyState>().expect("MyState not configured");
//! # }
//! # fn at_query_time(db: &AimDb) {
//! // Retrieving it from a live AimDb handle (query time):
//! let state = db.extensions().get::<MyState>().expect("MyState not configured");
//! # }
//! ```

use alloc::boxed::Box;
use core::any::{Any, TypeId};
use hashbrown::HashMap;

/// Generic extension storage for [`AimDbBuilder`][crate::AimDbBuilder] and
/// [`AimDb`][crate::AimDb].
///
/// Keyed by [`TypeId`] so each stored type occupies exactly one slot.
/// Values must be `Send + Sync + 'static` to be safe across thread and task
/// boundaries used throughout AimDB's async executor model.
pub struct Extensions {
    map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Default for Extensions {
    fn default() -> Self {
        Self::new()
    }
}

impl Extensions {
    /// Creates an empty extension map.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Inserts a value. Replaces any previously stored value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, val: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(val));
    }

    /// Returns a reference to the value of type `T`, or `None` if not stored.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref())
    }
}
