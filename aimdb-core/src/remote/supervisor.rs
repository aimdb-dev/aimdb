//! Remote access supervisor
//!
//! Manages the Unix domain socket server and spawns connection handlers for
//! remote clients connecting via the AimX protocol.

use crate::remote::AimxConfig;
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use std::os::unix::fs::PermissionsExt;

#[cfg(feature = "std")]
use tokio::net::UnixListener;

/// Spawns the remote access supervisor task
///
/// This function spawns a background task that:
/// 1. Binds to the Unix domain socket
/// 2. Sets appropriate file permissions
/// 3. Accepts incoming connections
/// 4. Spawns a ConnectionHandler for each client
///
/// # Arguments
/// * `db` - Database instance (for introspection and subscriptions)
/// * `runtime` - Runtime adapter (for spawning tasks)
/// * `config` - Remote access configuration
///
/// # Returns
/// `DbResult<()>` - Ok if supervisor spawned successfully
///
/// # Errors
/// Returns error if:
/// - Socket path already exists and cannot be removed
/// - Socket binding fails
/// - Permission setting fails
#[cfg(feature = "std")]
pub fn spawn_supervisor<R>(db: Arc<AimDb<R>>, runtime: Arc<R>, config: AimxConfig) -> DbResult<()>
where
    R: aimdb_executor::Spawn + 'static,
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

    // Spawn supervisor task using runtime adapter
    let _ = runtime.spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::info!("Remote access supervisor task started");

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    #[cfg(feature = "tracing")]
                    tracing::debug!("Accepted new client connection");

                    let db_clone = db.clone();
                    let config_clone = config.clone();

                    // Spawn connection handler for this client
                    tokio::spawn(async move {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("Connection handler spawned for client");

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
                    });
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to accept connection: {}", _e);
                    // Continue accepting other connections despite error
                }
            }
        }
    });

    Ok(())
}
