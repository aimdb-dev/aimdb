//! Axum WebSocket server and upgrade handler.
//!
//! The server is started by [`start_server`] which binds to the configured
//! address, mounts the WebSocket endpoint at the configured path, and
//! optionally mounts additional user-provided Axum routes.
//!
//! # Health endpoint
//!
//! `GET /health` returns `200 OK` with a JSON body:
//! ```json
//! { "status": "ok", "clients": 3, "uptime_secs": 120 }
//! ```

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use aimdb_core::{
    session::{run_session, SessionConfig},
    Connection, Dispatch, PeerInfo, SessionLimits,
};
use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use tower_http::cors::CorsLayer;

use crate::{
    auth::{AuthError, AuthRequest, ClientInfo, DynAuthHandler},
    client_manager::ClientManager,
    codec::WsCodec,
    transport::WsServerConnection,
};

// ════════════════════════════════════════════════════════════════════
// Shared server state
// ════════════════════════════════════════════════════════════════════

/// State shared across upgrade/health handlers. The per-connection session engine
/// (`run_session`) is driven from [`ws_upgrade_handler`]; only the *accept* loop
/// stays axum's (Option A, doc 039 § 6).
#[derive(Clone)]
pub(crate) struct ServerState {
    /// Shared application dispatch (one `Arc<dyn Dispatch>` per server).
    pub dispatch: Arc<dyn Dispatch>,
    /// HTTP-upgrade authenticator (resolves identity before the engine runs).
    pub auth: DynAuthHandler,
    /// Bus + connection counter (for client-id allocation and `/health`).
    pub client_mgr: ClientManager,
    /// Patterns to auto-subscribe each client to on connect.
    pub auto_subscribe: Arc<Vec<String>>,
    /// Per-connection subscription cap.
    pub max_subs_per_connection: usize,
    pub started_at: Instant,
}

// ════════════════════════════════════════════════════════════════════
// Server start
// ════════════════════════════════════════════════════════════════════

type BoxFuture = std::pin::Pin<Box<dyn core::future::Future<Output = ()> + Send + 'static>>;

/// Builds the WebSocket Axum server future.
///
/// Returns a `BoxFuture` containing the `axum::serve()` accept loop. The
/// future is appended to the `AimDbRunner` accumulator (design 028 §"Connector
/// futures"). Per-connection handlers spawned by Axum internally continue to
/// use `tokio::spawn` — outside the scope of issue #88.
///
/// # Arguments
///
/// * `bind_addr` — TCP address to listen on.
/// * `ws_path` — URL path for the WebSocket endpoint (e.g., `"/ws"`).
/// * `session_ctx` — Shared session context (auth, router, client manager, …).
/// * `additional_routes` — Optional user-supplied Axum `Router` that is merged
///   into the server (useful for REST + WebSocket on the same port).
/// Build the axum application (WS upgrade + health, plus any extra routes).
///
/// Extracted so tests can serve the **real** app on a known ephemeral port
/// (`build_server_future` binds internally and does not surface the port).
pub(crate) fn build_app(
    ws_path: &str,
    state: ServerState,
    additional_routes: Option<Router>,
) -> Router {
    // Apply state first so the router becomes `Router<()>`, which can then be
    // merged with user-supplied `additional_routes: Router<()>` without a
    // type-parameter mismatch.
    let ws_app = Router::new()
        .route(ws_path, get(ws_upgrade_handler))
        .route("/health", get(health_handler))
        .with_state(state)
        .layer(CorsLayer::permissive());

    match additional_routes {
        Some(extra) => ws_app.merge(extra),
        None => ws_app,
    }
}

pub(crate) fn build_server_future(
    bind_addr: SocketAddr,
    ws_path: String,
    state: ServerState,
    additional_routes: Option<Router>,
) -> BoxFuture {
    let app = build_app(&ws_path, state, additional_routes);

    Box::pin(async move {
        let listener = match tokio::net::TcpListener::bind(bind_addr).await {
            Ok(l) => l,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::error!("WebSocket connector failed to bind {}: {}", bind_addr, _e);
                return;
            }
        };

        #[cfg(feature = "tracing")]
        tracing::info!("WebSocket connector listening on {}", bind_addr);

        if let Err(_e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            #[cfg(feature = "tracing")]
            tracing::error!("WebSocket server error: {}", _e);
        }
    })
}

// ════════════════════════════════════════════════════════════════════
// Handlers
// ════════════════════════════════════════════════════════════════════

/// WebSocket upgrade handler.
///
/// Performs authentication before agreeing to upgrade; rejects unauthenticated
/// connections with HTTP 401.
async fn ws_upgrade_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query_params): Query<HashMap<String, String>>,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    let auth_req = AuthRequest {
        headers,
        query_params,
        remote_addr,
    };

    // Authenticate at the HTTP upgrade — returns permissions or rejects (401).
    let permissions = match state.auth.authenticate(&auth_req).await {
        Ok(p) => p,
        Err(AuthError { message }) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("WebSocket auth rejected from {}: {}", remote_addr, message);
            return (StatusCode::UNAUTHORIZED, message).into_response();
        }
    };

    // Resolve identity synchronously, before the upgrade, and carry it into the
    // engine via `PeerInfo::ext` (WS-style `reads_hello:false`, doc 039 § 4).
    let id = state.client_mgr.next_client_id();
    let info = ClientInfo {
        id,
        remote_addr,
        permissions,
    };

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "{}: upgrading WebSocket connection from {}",
        id,
        remote_addr
    );

    let dispatch = state.dispatch.clone();
    let auto_subscribe = state.auto_subscribe.clone();
    let config = SessionConfig {
        limits: SessionLimits {
            max_connections: usize::MAX, // axum owns the accept loop (Option A)
            max_subs_per_connection: state.max_subs_per_connection,
        },
        reads_hello: false,
        acks_subscribe: true,
    };

    ws.on_upgrade(move |socket: WebSocket| async move {
        let peer = PeerInfo::default().with_ext(Arc::new(info));
        let conn: Box<dyn Connection> =
            Box::new(WsServerConnection::new(socket, peer, &auto_subscribe));
        let codec = WsCodec::new();
        // Per-connection codec + run_session drive this socket (doc 039 § 6).
        run_session(conn, &codec, dispatch.as_ref(), &config).await;
    })
    .into_response()
}

/// Health check endpoint.
async fn health_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let clients = state.client_mgr.client_count();

    Json(serde_json::json!({
        "status": "ok",
        "clients": clients,
        "uptime_secs": uptime_secs,
    }))
}
