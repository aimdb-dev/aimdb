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

use std::{collections::HashMap, net::SocketAddr, time::Instant};

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
    auth::{AuthError, AuthRequest, ClientInfo},
    session::{run_session, SessionContext},
};

// ════════════════════════════════════════════════════════════════════
// Shared server state
// ════════════════════════════════════════════════════════════════════

#[derive(Clone)]
pub(crate) struct ServerState {
    pub session_ctx: SessionContext,
    pub started_at: Instant,
}

// ════════════════════════════════════════════════════════════════════
// Server start
// ════════════════════════════════════════════════════════════════════

/// Start the WebSocket Axum server and return immediately (the server runs in
/// a background Tokio task).
///
/// # Arguments
///
/// * `bind_addr` — TCP address to listen on.
/// * `ws_path` — URL path for the WebSocket endpoint (e.g., `"/ws"`).
/// * `session_ctx` — Shared session context (auth, router, client manager, …).
/// * `additional_routes` — Optional user-supplied Axum `Router` that is merged
///   into the server (useful for REST + WebSocket on the same port).
pub(crate) fn start_server(
    bind_addr: SocketAddr,
    ws_path: String,
    session_ctx: SessionContext,
    additional_routes: Option<Router>,
) {
    let state = ServerState {
        session_ctx,
        started_at: Instant::now(),
    };

    // Apply state first so the router becomes `Router<()>`, which can then be
    // merged with user-supplied `additional_routes: Router<()>` without a
    // type-parameter mismatch.
    let ws_app = Router::new()
        .route(&ws_path, get(ws_upgrade_handler))
        .route("/health", get(health_handler))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let app = if let Some(extra) = additional_routes {
        ws_app.merge(extra)
    } else {
        ws_app
    };

    tokio::spawn(async move {
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
    });
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

    // Authenticate — returns permissions or rejects
    let permissions = match state.session_ctx.auth.authenticate(&auth_req).await {
        Ok(p) => p,
        Err(AuthError { message }) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("WebSocket auth rejected from {}: {}", remote_addr, message);
            return (StatusCode::UNAUTHORIZED, message).into_response();
        }
    };

    // Allocate a client id before upgrading so it's available synchronously
    let id = state.session_ctx.client_mgr.next_client_id();
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

    let ctx = state.session_ctx.clone();

    ws.on_upgrade(move |socket: WebSocket| run_session(socket, info, ctx))
        .into_response()
}

/// Health check endpoint.
async fn health_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let clients = state.session_ctx.client_mgr.client_count();

    Json(serde_json::json!({
        "status": "ok",
        "clients": clients,
        "uptime_secs": uptime_secs,
    }))
}
