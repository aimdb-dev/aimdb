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
//!
//! # Version endpoint
//!
//! `GET /version` returns `200 OK` with the AimX major.minor this server
//! speaks:
//! ```json
//! { "aimx": "3.0" }
//! ```
//!
//! It exists because the upgrade-time version gate below answers an
//! incompatible client with HTTP 426, and a browser cannot read that: the
//! WebSocket API surfaces a failed upgrade as an opaque `error` event with no
//! status and no body, so a refusing hub and an unreachable one are
//! indistinguishable from JavaScript. A browser client fetches this route
//! before dialing and can then name both versions in its error.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use aimdb_core::{
    remote::{version_compatible, PROTOCOL_VERSION, VERSION_PARAM},
    session::{aimx::AimxCodec, run_session, SessionConfig},
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

use crate::transport::WsServerConnection;

use super::{
    auth::{AuthError, AuthRequest, ClientInfo, DynAuthHandler},
    client_manager::ClientManager,
};

// ════════════════════════════════════════════════════════════════════
// Shared server state
// ════════════════════════════════════════════════════════════════════

/// State shared across upgrade/health handlers. The per-connection session engine
/// (`run_session`) is driven from [`ws_upgrade_handler`]; only the *accept* loop
/// stays axum's.
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

/// Assemble the axum `Router`: the WS endpoint + `/health` + `/version`, with the shared
/// [`ServerState`] and any user-supplied extra routes merged in.
fn build_app(ws_path: &str, state: ServerState, additional_routes: Option<Router>) -> Router {
    // Apply state first so the router becomes `Router<()>`, which can then be
    // merged with user-supplied `additional_routes: Router<()>` without a
    // type-parameter mismatch.
    let ws_app = Router::new()
        .route(ws_path, get(ws_upgrade_handler))
        .route("/health", get(health_handler))
        .route("/version", get(version_handler))
        .with_state(state)
        .layer(CorsLayer::permissive());

    match additional_routes {
        Some(extra) => ws_app.merge(extra),
        None => ws_app,
    }
}

/// Bind `bind_addr` and serve [`build_app`] as the connector's runner future
/// (the axum accept loop). Each upgraded socket is driven by `run_session`.
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

    // Protocol-version gate, before auth: the socket transports negotiate the
    // version inside `hello`, but the WS server runs `reads_hello:false` and a
    // browser cannot set handshake headers — so the client declares its version
    // in the URL (`?v=3.0`, see `ws_url_with_version`). A missing or
    // major-incompatible version is refused here with 426 so a stale client
    // fails at the upgrade rather than on its first frame's new shape. Absent
    // fails closed (`version_compatible("")` is false), matching the socket gate.
    let client_version = auth_req
        .query_params
        .get(VERSION_PARAM)
        .map(String::as_str)
        .unwrap_or_default();
    if !version_compatible(client_version) {
        #[cfg(feature = "tracing")]
        tracing::warn!(
            "WebSocket upgrade from {} refused: incompatible protocol version {:?} (server speaks {})",
            remote_addr,
            client_version,
            PROTOCOL_VERSION
        );
        return (
            StatusCode::UPGRADE_REQUIRED,
            format!("incompatible AimX protocol version (server speaks {PROTOCOL_VERSION})"),
        )
            .into_response();
    }

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
    // engine via `PeerInfo::ext` (WS-style `reads_hello:false`).
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
            max_connections: usize::MAX, // axum owns the accept loop
            max_subs_per_connection: state.max_subs_per_connection,
        },
        reads_hello: false,
        acks_subscribe: true,
    };

    ws.on_upgrade(move |socket: WebSocket| async move {
        let peer = PeerInfo::default().with_ext(Arc::new(info));
        let conn: Box<dyn Connection> =
            Box::new(WsServerConnection::new(socket, peer, &auto_subscribe));
        // The shared AimX codec + run_session drive this socket; each codec blob
        // rides as one WS text frame.
        run_session(conn, &AimxCodec, dispatch.as_ref(), &config).await;
    })
    .into_response()
}

/// Protocol-version endpoint — the readable half of the upgrade gate.
///
/// Returns [`PROTOCOL_VERSION`] as `{"aimx": "3.0"}`. Unlike the 426 the gate
/// returns, this is reachable from a browser (the router's permissive CORS
/// layer covers it), so a WASM/JS client can learn what the hub speaks before
/// it dials and report a version mismatch instead of a bare connection
/// failure. The body is exactly one field so the shape stays cheap to depend
/// on; the same major-version rule as [`version_compatible`] applies to it.
async fn version_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "aimx": PROTOCOL_VERSION }))
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
