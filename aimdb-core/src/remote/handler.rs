//! Connection handler for AimX protocol
//!
//! Handles individual client connections, including handshake, authentication,
//! and protocol method dispatch.

use crate::remote::{AimxConfig, HelloMessage, RecordMetadata, Request, Response, WelcomeMessage};
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use serde_json::json;
#[cfg(feature = "std")]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(feature = "std")]
use tokio::net::UnixStream;

/// Handles an incoming client connection
///
/// Processes the AimX protocol handshake and manages the client session.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration
/// * `stream` - Unix domain socket stream
///
/// # Errors
/// Returns error if handshake fails or stream operations error
#[cfg(feature = "std")]
pub async fn handle_connection<R>(
    db: Arc<AimDb<R>>,
    config: AimxConfig,
    stream: UnixStream,
) -> DbResult<()>
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::info!("New remote access connection established");

    // Perform protocol handshake
    let mut stream = match perform_handshake(stream, &config, &db).await {
        Ok(stream) => stream,
        Err(e) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Handshake failed: {}", e);
            return Err(e);
        }
    };

    #[cfg(feature = "tracing")]
    tracing::info!("Handshake complete, client ready");

    // Request/response loop
    loop {
        let mut line = String::new();

        match stream.read_line(&mut line).await {
            Ok(0) => {
                // Client closed connection
                #[cfg(feature = "tracing")]
                tracing::info!("Client disconnected gracefully");
                break;
            }
            Ok(_) => {
                #[cfg(feature = "tracing")]
                tracing::debug!("Received request: {}", line.trim());

                // Parse request
                let request: Request = match serde_json::from_str(line.trim()) {
                    Ok(req) => req,
                    Err(e) => {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Failed to parse request: {}", e);

                        // Send error response (use ID 0 if we can't parse the request)
                        let error_response =
                            Response::error(0, "parse_error", format!("Invalid JSON: {}", e));
                        if let Err(_e) = send_response(&mut stream, &error_response).await {
                            #[cfg(feature = "tracing")]
                            tracing::error!("Failed to send error response: {}", _e);
                            break;
                        }
                        continue;
                    }
                };

                // Dispatch request to appropriate handler
                let response = handle_request(&db, &config, request).await;

                // Send response
                if let Err(_e) = send_response(&mut stream, &response).await {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to send response: {}", _e);
                    break;
                }
            }
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::error!("Error reading from stream: {}", _e);
                break;
            }
        }
    }

    #[cfg(feature = "tracing")]
    tracing::info!("Connection handler terminating");

    Ok(())
}

/// Performs the AimX protocol handshake
///
/// Handshake flow:
/// 1. Client sends HelloMessage with protocol version
/// 2. Server validates version compatibility
/// 3. Server sends WelcomeMessage with accepted version
/// 4. Optional: Authenticate with token
///
/// # Arguments
/// * `stream` - Unix domain socket stream
/// * `config` - Remote access configuration
/// * `db` - Database instance (for querying writable records)
///
/// # Returns
/// `BufReader<UnixStream>` if handshake succeeds
///
/// # Errors
/// Returns error if:
/// - Protocol version incompatible
/// - Authentication fails
/// - IO error during handshake
#[cfg(feature = "std")]
async fn perform_handshake<R>(
    stream: UnixStream,
    config: &AimxConfig,
    db: &Arc<AimDb<R>>,
) -> DbResult<BufReader<UnixStream>>
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Read Hello message from client
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to read Hello message".to_string(),
            source: e,
        })?;

    #[cfg(feature = "tracing")]
    tracing::debug!("Received handshake: {}", line.trim());

    // Parse Hello message
    let hello: HelloMessage =
        serde_json::from_str(line.trim()).map_err(|e| DbError::JsonWithContext {
            context: "Failed to parse Hello message".to_string(),
            source: e,
        })?;

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Client hello: version={}, client={}",
        hello.version,
        hello.client
    );

    // Version validation: accept "1.0" or "1"
    if hello.version != "1.0" && hello.version != "1" {
        let error_msg = format!(
            r#"{{"error":"unsupported_version","message":"Server supports version 1.0, client requested {}"}}"#,
            hello.version
        );

        #[cfg(feature = "tracing")]
        tracing::warn!("Unsupported version: {}", hello.version);

        let _ = writer.write_all(error_msg.as_bytes()).await;
        let _ = writer.write_all(b"\n").await;
        let _ = writer.shutdown().await;

        return Err(DbError::InvalidOperation {
            operation: "handshake".to_string(),
            reason: format!("Unsupported version: {}", hello.version),
        });
    }

    // Check authentication if required
    let authenticated = if let Some(expected_token) = &config.auth_token {
        match &hello.auth_token {
            Some(provided_token) if provided_token == expected_token => {
                #[cfg(feature = "tracing")]
                tracing::debug!("Authentication successful");
                true
            }
            Some(_) => {
                let error_msg =
                    r#"{"error":"authentication_failed","message":"Invalid auth token"}"#;

                #[cfg(feature = "tracing")]
                tracing::warn!("Authentication failed: invalid token");

                let _ = writer.write_all(error_msg.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                let _ = writer.shutdown().await;

                return Err(DbError::PermissionDenied {
                    operation: "authentication".to_string(),
                });
            }
            None => {
                let error_msg =
                    r#"{"error":"authentication_required","message":"Auth token required"}"#;

                #[cfg(feature = "tracing")]
                tracing::warn!("Authentication failed: no token provided");

                let _ = writer.write_all(error_msg.as_bytes()).await;
                let _ = writer.write_all(b"\n").await;
                let _ = writer.shutdown().await;

                return Err(DbError::PermissionDenied {
                    operation: "authentication".to_string(),
                });
            }
        }
    } else {
        false
    };

    // Determine permissions based on security policy
    let permissions = match &config.security_policy {
        crate::remote::SecurityPolicy::ReadOnly => vec!["read".to_string()],
        crate::remote::SecurityPolicy::ReadWrite { .. } => {
            vec!["read".to_string(), "write".to_string()]
        }
    };

    // Get writable records by querying database for writable record names
    let writable_records = match &config.security_policy {
        crate::remote::SecurityPolicy::ReadOnly => vec![],
        crate::remote::SecurityPolicy::ReadWrite {
            writable_records: _writable_type_ids,
        } => {
            // Get all records from database
            let all_records: Vec<RecordMetadata> = db.list_records();

            // Filter to those that are marked writable
            all_records
                .into_iter()
                .filter(|meta| meta.writable)
                .map(|meta| meta.name)
                .collect()
        }
    };

    // Send Welcome message
    let welcome = WelcomeMessage {
        version: "1.0".to_string(),
        server: "aimdb".to_string(),
        permissions,
        writable_records,
        max_subscriptions: Some(config.subscription_queue_size),
        authenticated: Some(authenticated),
    };

    let welcome_json = serde_json::to_string(&welcome).map_err(|e| DbError::JsonWithContext {
        context: "Failed to serialize Welcome message".to_string(),
        source: e,
    })?;

    writer
        .write_all(welcome_json.as_bytes())
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write Welcome message".to_string(),
            source: e,
        })?;

    writer
        .write_all(b"\n")
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write Welcome newline".to_string(),
            source: e,
        })?;

    #[cfg(feature = "tracing")]
    tracing::info!("Sent Welcome message to client");

    // Reunite the stream
    let stream = reader
        .into_inner()
        .reunite(writer)
        .map_err(|e| DbError::Io {
            source: std::io::Error::other(e.to_string()),
        })?;

    Ok(BufReader::new(stream))
}

/// Handles a single request and returns a response
///
/// Dispatches to the appropriate handler based on the request method.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration
/// * `request` - The parsed request
///
/// # Returns
/// Response to send to the client
#[cfg(feature = "std")]
async fn handle_request<R>(db: &Arc<AimDb<R>>, config: &AimxConfig, request: Request) -> Response
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Handling request: method={}, id={}",
        request.method,
        request.id
    );

    match request.method.as_str() {
        "record.list" => handle_record_list(db, config, request.id).await,
        "record.get" => handle_record_get(db, config, request.id, request.params).await,
        _ => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Unknown method: {}", request.method);

            Response::error(
                request.id,
                "method_not_found",
                format!("Unknown method: {}", request.method),
            )
        }
    }
}

/// Handles record.list method
///
/// Returns metadata for all registered records in the database.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration (for permission checks)
/// * `request_id` - Request ID for the response
///
/// # Returns
/// Success response with array of RecordMetadata
#[cfg(feature = "std")]
async fn handle_record_list<R>(
    db: &Arc<AimDb<R>>,
    _config: &AimxConfig,
    request_id: u64,
) -> Response
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!("Listing records");

    // Get all record metadata from database
    let records: Vec<RecordMetadata> = db.list_records();

    #[cfg(feature = "tracing")]
    tracing::debug!("Found {} records", records.len());

    // Convert to JSON and return
    Response::success(request_id, json!(records))
}

/// Handles record.get method
///
/// Returns the current value of a record as JSON.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration (for permission checks)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "record" field with record name)
///
/// # Returns
/// Success response with record value as JSON, or error if:
/// - Missing/invalid "record" parameter
/// - Record not found
/// - Record not configured with `.with_serialization()`
/// - No value available in atomic snapshot
#[cfg(feature = "std")]
async fn handle_record_get<R>(
    db: &Arc<AimDb<R>>,
    _config: &AimxConfig,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    // Extract record name from params
    let record_name = match params {
        Some(serde_json::Value::Object(map)) => match map.get("record") {
            Some(serde_json::Value::String(name)) => name.clone(),
            _ => {
                #[cfg(feature = "tracing")]
                tracing::warn!("Missing or invalid 'record' parameter");

                return Response::error(
                    request_id,
                    "invalid_params",
                    "Missing or invalid 'record' parameter".to_string(),
                );
            }
        },
        _ => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Missing params object");

            return Response::error(
                request_id,
                "invalid_params",
                "Missing params object".to_string(),
            );
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!("Getting value for record: {}", record_name);

    // Try to peek the record's JSON value
    match db.try_latest_as_json(&record_name) {
        Some(value) => {
            #[cfg(feature = "tracing")]
            tracing::debug!("Successfully retrieved value for {}", record_name);

            Response::success(request_id, value)
        }
        None => {
            #[cfg(feature = "tracing")]
            tracing::warn!("No value available for record: {}", record_name);

            Response::error(
                request_id,
                "not_found",
                format!("No value available for record: {}", record_name),
            )
        }
    }
}

/// Sends a response to the client
///
/// Serializes the response to JSON and writes it to the stream with a newline.
///
/// # Arguments
/// * `stream` - The connection stream
/// * `response` - The response to send
///
/// # Errors
/// Returns error if serialization or write fails
#[cfg(feature = "std")]
async fn send_response(stream: &mut BufReader<UnixStream>, response: &Response) -> DbResult<()> {
    let response_json = serde_json::to_string(response).map_err(|e| DbError::JsonWithContext {
        context: "Failed to serialize response".to_string(),
        source: e,
    })?;

    stream
        .get_mut()
        .write_all(response_json.as_bytes())
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write response".to_string(),
            source: e,
        })?;

    stream
        .get_mut()
        .write_all(b"\n")
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write response newline".to_string(),
            source: e,
        })?;

    #[cfg(feature = "tracing")]
    tracing::debug!("Sent response");

    Ok(())
}
