//! Outbound path: connectors pull serialized messages from their routes.

mod ready;
mod routes;

pub use routes::{OutboundMessage, OutboundPayload, OutboundRoutes, RouteInfo, RouteStats};
pub(crate) use routes::{PollRoute, RouteFactoryFn, RouteOutcome, RouteParts, StagedPayload};

/// Dense route index, `0..routes().len()`. A plain `usize`, so a connector
/// indexes its own per-route tables with it.
pub type RouteId = usize;
