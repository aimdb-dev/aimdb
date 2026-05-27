//! Remote access supervisor
//!
//! Manages the Unix domain socket server and drives per-connection
//! handlers for remote clients connecting via the AimX protocol. Each
//! accepted connection is pushed onto a per-supervisor
//! [`FuturesUnordered`]; there is no `tokio::spawn`.

use crate::builder::BoxFuture;
use crate::remote::AimxConfig;
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use std::os::unix::fs::PermissionsExt;

#[cfg(feature = "std")]
use futures_util::stream::{FuturesUnordered, StreamExt};
#[cfg(feature = "std")]
use tokio::net::UnixListener;

/// Builds the remote access supervisor future.
///
/// Synchronously: binds the Unix domain socket and sets file permissions
/// (so binding errors surface from `build()` rather than at task-start time).
///
/// The returned [`BoxFuture`] is appended to the `AimDbRunner` accumulator;
/// when driven, it accepts incoming connections in a loop and pushes each
/// per-connection handler onto a [`FuturesUnordered`]. `tokio::select!`
/// with `biased;` keeps `accept` polled ahead of connection drains so a
/// chatty client cannot starve new connects.
///
/// # Arguments
/// * `db` - Database instance (for introspection and subscriptions)
/// * `config` - Remote access configuration
///
/// # Errors
/// Returns error if:
/// - Socket path already exists and cannot be removed
/// - Socket binding fails
/// - Permission setting fails
#[cfg(feature = "std")]
pub fn build_supervisor_future<R>(db: Arc<AimDb<R>>, config: AimxConfig) -> DbResult<BoxFuture>
where
    R: aimdb_executor::RuntimeAdapter + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::info!(
        "Initializing remote access supervisor on socket: {}",
        config.socket_path.display()
    );

    // Remove existing socket file if it exists
    if config.socket_path.exists() {
        #[cfg(feature = "tracing")]
        tracing::debug!(
            "Removing existing socket file: {}",
            config.socket_path.display()
        );

        std::fs::remove_file(&config.socket_path).map_err(|e| DbError::IoWithContext {
            context: format!(
                "Failed to remove existing socket file {}",
                config.socket_path.display()
            ),
            source: e,
        })?;
    }

    // Bind to Unix domain socket
    let listener = UnixListener::bind(&config.socket_path).map_err(|e| DbError::IoWithContext {
        context: format!(
            "Failed to bind Unix socket at {}",
            config.socket_path.display()
        ),
        source: e,
    })?;

    #[cfg(feature = "tracing")]
    tracing::info!(
        "Unix socket bound successfully: {}",
        config.socket_path.display()
    );

    // Set socket file permissions
    let mut perms = std::fs::metadata(&config.socket_path)
        .map_err(|e| DbError::IoWithContext {
            context: format!(
                "Failed to read socket metadata for {}",
                config.socket_path.display()
            ),
            source: e,
        })?
        .permissions();

    let permissions = config.socket_permissions.unwrap_or(0o600);
    perms.set_mode(permissions);

    std::fs::set_permissions(&config.socket_path, perms).map_err(|e| DbError::IoWithContext {
        context: format!(
            "Failed to set socket permissions for {}",
            config.socket_path.display()
        ),
        source: e,
    })?;

    #[cfg(feature = "tracing")]
    tracing::info!("Socket permissions set to {:o}", permissions);

    // The accept loop is the future the runner drives. Per-connection
    // handler futures live in a `FuturesUnordered` owned by this future;
    // dropping the supervisor (e.g. when the runner is cancelled) drops
    // every active connection in turn.
    let supervisor_future: BoxFuture = Box::pin(async move {
        #[cfg(feature = "tracing")]
        tracing::info!("Remote access supervisor task started");

        let mut connections: FuturesUnordered<BoxFuture> = FuturesUnordered::new();

        loop {
            tokio::select! {
                biased;

                // Accept the next incoming connection
                accept_res = listener.accept() => match accept_res {
                    Ok((stream, _addr)) => {
                        // Refuse if we are already at the connection cap.
                        // The accepted `UnixStream` is dropped, which closes
                        // the socket; the client sees a closed connection.
                        if connections.len() >= config.max_connections {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(
                                "max_connections={} reached, refusing new client",
                                config.max_connections
                            );
                            drop(stream);
                            continue;
                        }

                        #[cfg(feature = "tracing")]
                        tracing::debug!("Accepted new client connection");

                        let db_clone = db.clone();
                        let config_clone = config.clone();
                        connections.push(Box::pin(async move {
                            if let Err(_e) = crate::remote::handler::handle_connection(
                                db_clone,
                                config_clone,
                                stream,
                            )
                            .await
                            {
                                #[cfg(feature = "tracing")]
                                tracing::error!("Connection handler error: {}", _e);
                            }

                            #[cfg(feature = "tracing")]
                            tracing::debug!("Connection handler terminated");
                        }));
                    }
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Failed to accept connection: {}", _e);
                        // Continue accepting other connections despite error
                    }
                },

                // Drain finished connection futures. Using `Some(_) = next()`
                // (rather than `select_next_some()`) is the safe form: an
                // empty `FuturesUnordered` reports `is_terminated() == true`,
                // and `select_next_some` panics in that state. With the
                // pattern guard, the arm is simply disabled when `next()`
                // resolves to `None`, and the always-active `accept`
                // arm keeps the select alive.
                Some(_) = connections.next() => {}
            }
        }
    });

    Ok(supervisor_future)
}
