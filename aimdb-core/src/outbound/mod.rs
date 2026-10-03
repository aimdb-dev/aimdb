//! Outbound path: connectors pull serialized messages from their routes.

mod ready;

/// Dense route index, `0..len`.
pub(crate) type RouteId = usize;
