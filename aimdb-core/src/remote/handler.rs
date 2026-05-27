//! Connection handler for AimX protocol
//!
//! Handles individual client connections, including handshake, authentication,
//! and protocol method dispatch.
//!
//! # Architecture: Event Funnel Pattern
//!
//! Subscriptions use a funnel pattern for clean event delivery:
//! - Each `record.subscribe` pushes a future onto a per-connection
//!   [`futures_util::stream::FuturesUnordered`] that the connection's
//!   outer `select!` loop drives.
//! - Subscription futures send events to a shared mpsc channel (the "funnel").
//! - The same outer loop drains the funnel and writes events to the
//!   `UnixStream`, so NDJSON line integrity is preserved without a
//!   dedicated writer task.

use crate::remote::{
    AimxConfig, Event, HelloMessage, RecordMetadata, Request, Response, WelcomeMessage,
};
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use std::collections::HashMap;
#[cfg(feature = "std")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use futures_core::Stream;
#[cfg(feature = "std")]
use futures_util::stream::{FuturesUnordered, StreamExt};
#[cfg(feature = "std")]
use serde_json::json;
#[cfg(feature = "std")]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(feature = "std")]
use tokio::net::UnixStream;
#[cfg(feature = "std")]
use tokio::sync::mpsc;

#[cfg(feature = "std")]
use crate::builder::BoxFuture;

/// Connection state for managing subscriptions
///
/// Tracks all active subscriptions for a single client connection. Each
/// subscription is identified by its `subscription_id` and carries an
/// `Arc<AtomicBool>` that the `record.unsubscribe` handler flips to signal
/// the per-subscription future to exit on its next poll. Connection
/// teardown does not need to flip the flag — dropping the per-connection
/// `FuturesUnordered` (in [`handle_connection`]) drops every subscription
/// future, which is the primary cancellation path.
#[cfg(feature = "std")]
struct ConnectionState {
    /// Active subscriptions by subscription_id → cancel flag.
    subscriptions: HashMap<String, Arc<AtomicBool>>,

    /// Counter for generating unique subscription IDs
    next_subscription_id: u64,

    /// Event funnel: all subscription futures send events here.
    /// This channel feeds the connection's send loop.
    event_tx: mpsc::UnboundedSender<Event>,

    /// Per-record drain readers, created lazily on first record.drain call.
    /// One drain reader per record, per connection.
    drain_readers: HashMap<String, Box<dyn crate::buffer::JsonBufferReader + Send>>,
}

#[cfg(feature = "std")]
impl ConnectionState {
    /// Creates a new connection state
    fn new(event_tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            subscriptions: HashMap::new(),
            next_subscription_id: 1,
            event_tx,
            drain_readers: HashMap::new(),
        }
    }

    /// Generates a unique subscription ID for this connection
    fn generate_subscription_id(&mut self) -> String {
        let id = format!("sub-{}", self.next_subscription_id);
        self.next_subscription_id += 1;
        id
    }
}

/// Handles an incoming client connection
///
/// Processes the AimX protocol handshake and manages the client session.
/// Implements the event funnel pattern for subscription event delivery.
///
/// # Architecture
///
/// ```text
/// ┌──────────────────────┐
/// │ Subscription future 1│───┐
/// │ (in FuturesUnordered)│   │
/// └──────────────────────┘   │
///                            ├──► Event Funnel ───► select! loop ───► UnixStream
/// ┌──────────────────────┐   │     (mpsc)          (interleaved
/// │ Subscription future 2│───┘                      writes)
/// │ (in FuturesUnordered)│
/// └──────────────────────┘
/// ```
///
/// The main loop uses `tokio::select! { biased; }` to interleave:
/// - Reading requests from the stream
/// - Writing events from subscriptions
/// - Draining completed subscription futures
///
/// `biased;` polls the request arm first so a chatty subscription
/// cannot starve the request path. Cancellation is by drop: when the
/// outer loop exits, the per-connection `FuturesUnordered` is dropped
/// and every subscription future with it.
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
    R: crate::RuntimeAdapter + 'static,
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

    // Create event funnel: all subscription futures send events here
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();

    // Initialize connection state
    let mut conn_state = ConnectionState::new(event_tx);

    // Per-connection FuturesUnordered of subscription futures. Each
    // `record.subscribe` pushes one future here; cancellation flows
    // through `Arc<AtomicBool>` (Unsubscribe) or by dropping `subs` when
    // the connection ends.
    let mut subs: FuturesUnordered<BoxFuture> = FuturesUnordered::new();

    // Main loop: interleave reading requests, writing events, and draining
    // completed subscription futures. `biased;` keeps request reads polled
    // first so a chatty subscription cannot starve the request path.
    loop {
        let mut line = String::new();

        tokio::select! {
            biased;

            // Handle incoming requests
            read_result = stream.read_line(&mut line) => {
                match read_result {
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
                        let response = handle_request(
                            &db,
                            &config,
                            &mut conn_state,
                            &mut subs,
                            request,
                        )
                        .await;

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

            // Handle outgoing events from subscriptions
            Some(event) = event_rx.recv() => {
                if let Err(_e) = send_event(&mut stream, &event).await {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to send event: {}", _e);
                    break;
                }
            }

            // Drain finished subscription futures so `subs` does not grow
            // unboundedly. Using `Some(_) = next()` (rather than
            // `select_next_some()`) is the safe form: an empty
            // `FuturesUnordered` reports `is_terminated() == true`, and
            // `select_next_some` panics in that state. With the pattern
            // guard the arm is simply disabled when `next()` resolves to
            // `None`, and the always-active `read_line` arm keeps the
            // select alive.
            Some(_) = subs.next() => {}
        }
    }

    // Dropping `subs` here cancels every still-running subscription future
    // — the connection's `FuturesUnordered` is their sole owner.
    drop(subs);

    #[cfg(feature = "tracing")]
    tracing::info!("Connection handler terminating");

    Ok(())
}

/// Sends an event to the client
///
/// Serializes the event to JSON and writes it to the stream with a newline.
///
/// # Arguments
/// * `stream` - The connection stream
/// * `event` - The event to send
///
/// # Errors
/// Returns error if serialization or write fails
#[cfg(feature = "std")]
async fn send_event(stream: &mut BufReader<UnixStream>, event: &Event) -> DbResult<()> {
    // Wrap event in protocol envelope
    let event_msg = json!({ "event": event });

    let event_json = serde_json::to_string(&event_msg).map_err(|e| DbError::JsonWithContext {
        context: "Failed to serialize event".to_string(),
        source: e,
    })?;

    stream
        .get_mut()
        .write_all(event_json.as_bytes())
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write event".to_string(),
            source: e,
        })?;

    stream
        .get_mut()
        .write_all(b"\n")
        .await
        .map_err(|e| DbError::IoWithContext {
            context: "Failed to write event newline".to_string(),
            source: e,
        })?;

    #[cfg(feature = "tracing")]
    tracing::trace!("Sent event for subscription: {}", event.subscription_id);

    Ok(())
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
    R: crate::RuntimeAdapter + 'static,
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
        max_subscriptions: Some(config.max_subs_per_connection),
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
/// * `conn_state` - Connection state (for subscription management)
/// * `request` - The parsed request
///
/// # Returns
/// Response to send to the client
#[cfg(feature = "std")]
async fn handle_request<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    conn_state: &mut ConnectionState,
    subs: &mut FuturesUnordered<BoxFuture>,
    request: Request,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
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
        "record.set" => handle_record_set(db, config, request.id, request.params).await,
        "record.subscribe" => {
            handle_record_subscribe(db, config, conn_state, subs, request.id, request.params).await
        }
        "record.unsubscribe" => {
            handle_record_unsubscribe(conn_state, request.id, request.params).await
        }
        "record.drain" => handle_record_drain(db, conn_state, request.id, request.params).await,
        "record.query" => handle_record_query(db, request.id, request.params).await,
        "graph.nodes" => handle_graph_nodes(db, request.id).await,
        "graph.edges" => handle_graph_edges(db, request.id).await,
        "graph.topo_order" => handle_graph_topo_order(db, request.id).await,
        #[cfg(feature = "profiling")]
        "profiling.reset" => handle_profiling_reset(db, config, request.id).await,
        #[cfg(feature = "metrics")]
        "buffer_metrics.reset" => handle_buffer_metrics_reset(db, config, request.id).await,
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
    R: crate::RuntimeAdapter + 'static,
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

/// Handles profiling.reset method
///
/// Clears stage profiling counters for every record. Requires write permission.
#[cfg(all(feature = "std", feature = "profiling"))]
async fn handle_profiling_reset<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    request_id: u64,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    if matches!(
        config.security_policy,
        crate::remote::SecurityPolicy::ReadOnly
    ) {
        return Response::error(
            request_id,
            "permission_denied",
            "profiling.reset requires write permission (ReadOnly security policy)".to_string(),
        );
    }

    db.reset_stage_profiling();

    #[cfg(feature = "tracing")]
    tracing::info!("Stage profiling counters reset");

    Response::success(request_id, json!({ "reset": true }))
}

/// Handles buffer_metrics.reset method
///
/// Clears buffer introspection counters for every record. Requires write permission.
#[cfg(all(feature = "std", feature = "metrics"))]
async fn handle_buffer_metrics_reset<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    request_id: u64,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    if matches!(
        config.security_policy,
        crate::remote::SecurityPolicy::ReadOnly
    ) {
        return Response::error(
            request_id,
            "permission_denied",
            "buffer_metrics.reset requires write permission (ReadOnly security policy)".to_string(),
        );
    }

    db.reset_buffer_metrics();

    #[cfg(feature = "tracing")]
    tracing::info!("Buffer metrics counters reset");

    Response::success(request_id, json!({ "reset": true }))
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
/// - Record not configured with `.with_remote_access()`
/// - No value available in atomic snapshot
#[cfg(feature = "std")]
async fn handle_record_get<R>(
    db: &Arc<AimDb<R>>,
    _config: &AimxConfig,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
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

/// Handles record.set method
///
/// Sets a record value from JSON (write operation).
///
/// **SAFETY:** Enforces the "No Producer Override" rule:
/// - Only allows writes to configuration records (producer_count == 0)
/// - Prevents remote access from interfering with application logic
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration (for permission checks)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "name" and "value" fields)
///
/// # Returns
/// Success response, or error if:
/// - Missing/invalid parameters
/// - Record not found
/// - Permission denied (not writable or has active producers)
/// - Deserialization failed
#[cfg(feature = "std")]
async fn handle_record_set<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    use crate::remote::SecurityPolicy;

    // Check if write operations are allowed
    let writable_records = match &config.security_policy {
        SecurityPolicy::ReadOnly => {
            #[cfg(feature = "tracing")]
            tracing::warn!("record.set called but security policy is ReadOnly");

            return Response::error(
                request_id,
                "permission_denied",
                "Write operations not allowed (ReadOnly security policy)".to_string(),
            );
        }
        SecurityPolicy::ReadWrite { writable_records } => writable_records,
    };

    // Extract record name and value from params
    let (record_name, value) = match params {
        Some(serde_json::Value::Object(ref map)) => {
            let name = match map.get("name") {
                Some(serde_json::Value::String(n)) => n.clone(),
                _ => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Missing or invalid 'name' parameter in record.set");

                    return Response::error(
                        request_id,
                        "invalid_params",
                        "Missing or invalid 'name' parameter (expected string)".to_string(),
                    );
                }
            };

            let val = match map.get("value") {
                Some(v) => v.clone(),
                None => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Missing 'value' parameter in record.set");

                    return Response::error(
                        request_id,
                        "invalid_params",
                        "Missing 'value' parameter".to_string(),
                    );
                }
            };

            (name, val)
        }
        _ => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Missing params object in record.set");

            return Response::error(
                request_id,
                "invalid_params",
                "Missing params object".to_string(),
            );
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!("Setting value for record: {}", record_name);

    // Check if record is in the writable_records set (using record key)
    if !writable_records.contains(&record_name) {
        #[cfg(feature = "tracing")]
        tracing::warn!("Record '{}' not in writable_records set", record_name);

        return Response::error(
            request_id,
            "permission_denied",
            format!(
                "Record '{}' is not writable. \
                 Configure with .with_writable_record() to allow writes.",
                record_name
            ),
        );
    }

    // Attempt to set the value
    // This will enforce the "no producer override" rule internally
    match db.set_record_from_json(&record_name, value) {
        Ok(()) => {
            #[cfg(feature = "tracing")]
            tracing::info!("Successfully set value for record: {}", record_name);

            // Get the updated value to return in response
            let result = if let Some(updated_value) = db.try_latest_as_json(&record_name) {
                serde_json::json!({
                    "status": "success",
                    "value": updated_value,
                })
            } else {
                serde_json::json!({
                    "status": "success",
                })
            };

            Response::success(request_id, result)
        }
        Err(e) => {
            #[cfg(feature = "tracing")]
            tracing::error!("Failed to set value for record '{}': {}", record_name, e);

            // Map internal errors to appropriate response codes
            let (code, message) = match e {
                crate::DbError::RecordKeyNotFound { key } => {
                    ("not_found", format!("Record '{}' not found", key))
                }
                crate::DbError::PermissionDenied { operation } => {
                    // This is the "has active producers" error
                    ("permission_denied", operation)
                }
                crate::DbError::JsonWithContext { context, .. } => (
                    "validation_error",
                    format!("JSON validation failed: {}", context),
                ),
                crate::DbError::RuntimeError { message } => ("internal_error", message),
                _ => ("internal_error", format!("Failed to set value: {}", e)),
            };

            Response::error(request_id, code, message)
        }
    }
}

/// Handles record.subscribe method
///
/// Subscribes to live updates for a record. Pushes a per-subscription
/// future onto the connection's [`FuturesUnordered`] (`subs`) — there is
/// no `tokio::spawn`; the connection's outer loop drives the future.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration
/// * `conn_state` - Connection state (for subscription tracking)
/// * `subs` - Per-connection set of subscription futures (this fn pushes one)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "name" field with record name)
///
/// # Returns
/// Success response with subscription_id or error if:
/// - Missing/invalid parameters
/// - Record not found
/// - Too many subscriptions
#[cfg(feature = "std")]
async fn handle_record_subscribe<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    conn_state: &mut ConnectionState,
    subs: &mut FuturesUnordered<BoxFuture>,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    // Extract record name from params
    let record_name = match params {
        Some(serde_json::Value::Object(ref map)) => match map.get("name") {
            Some(serde_json::Value::String(name)) => name.clone(),
            _ => {
                #[cfg(feature = "tracing")]
                tracing::warn!("Missing or invalid 'name' parameter in record.subscribe");

                return Response::error(
                    request_id,
                    "invalid_params",
                    "Missing or invalid 'name' parameter (expected string)".to_string(),
                );
            }
        },
        _ => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Missing params object in record.subscribe");

            return Response::error(
                request_id,
                "invalid_params",
                "Missing params object".to_string(),
            );
        }
    };

    // Optional: send_initial flag (default true)
    let _send_initial = params
        .as_ref()
        .and_then(|p| p.as_object())
        .and_then(|map| map.get("send_initial"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    #[cfg(feature = "tracing")]
    tracing::debug!("Subscribing to record: {}", record_name);

    // Check max subscriptions per connection.
    if conn_state.subscriptions.len() >= config.max_subs_per_connection {
        #[cfg(feature = "tracing")]
        tracing::warn!(
            "Too many subscriptions: {} (max: {})",
            conn_state.subscriptions.len(),
            config.max_subs_per_connection
        );

        return Response::error(
            request_id,
            "too_many_subscriptions",
            format!(
                "Maximum subscriptions reached: {}",
                config.max_subs_per_connection
            ),
        );
    }

    // Subscribe to the record's JSON event stream
    let value_stream = match crate::remote::stream::stream_record_updates(db, &record_name) {
        Ok(s) => s,
        Err(e) => {
            // Map internal errors to appropriate response codes
            let (code, message) = match &e {
                crate::DbError::RecordKeyNotFound { key } => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("Record not found: {}", key);
                    ("not_found", format!("Record '{}' not found", key))
                }
                _ => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to subscribe to record updates: {}", e);
                    ("internal_error", format!("Failed to subscribe: {}", e))
                }
            };

            return Response::error(request_id, code, message);
        }
    };

    // Generate unique subscription ID and cancel flag
    let subscription_id = conn_state.generate_subscription_id();
    let cancel = Arc::new(AtomicBool::new(false));

    // Push the subscription future onto the connection's set. The future
    // is dropped — and therefore cancelled — when either the cancel flag
    // is flipped (Unsubscribe) or the outer connection loop exits.
    let event_tx = conn_state.event_tx.clone();
    let sub_id_for_future = subscription_id.clone();
    let cancel_for_future = cancel.clone();
    subs.push(Box::pin(run_subscription(
        value_stream,
        sub_id_for_future,
        event_tx,
        cancel_for_future,
    )));

    conn_state
        .subscriptions
        .insert(subscription_id.clone(), cancel);

    #[cfg(feature = "tracing")]
    tracing::info!(
        "Created subscription {} for record {}",
        subscription_id,
        record_name
    );

    // Return success response
    Response::success(
        request_id,
        json!({
            "subscription_id": subscription_id,
        }),
    )
}

/// Per-subscription future: forwards JSON values from the record stream
/// into the connection's event funnel as `Event` messages with sequence
/// numbers and RFC-style "secs.nanos" timestamps.
///
/// Exits when any of:
/// - the `cancel` flag is set (by `record.unsubscribe`) — checked after
///   each stream poll, so cancellation has up to a one-event delay;
/// - the upstream stream ends (e.g. `BufferClosed`);
/// - the event funnel is closed (connection going down).
///
/// Connection-close cancellation does not rely on the flag — the
/// connection's `FuturesUnordered` is the sole owner of this future and
/// dropping the set drops the future.
#[cfg(feature = "std")]
async fn run_subscription<S>(
    stream: S,
    subscription_id: String,
    event_tx: mpsc::UnboundedSender<Event>,
    cancel: Arc<AtomicBool>,
) where
    S: Stream<Item = serde_json::Value> + Send + 'static,
{
    futures_util::pin_mut!(stream);
    let mut sequence: u64 = 1;

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Subscription future started for subscription: {}",
        subscription_id
    );

    while let Some(json_value) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            #[cfg(feature = "tracing")]
            tracing::debug!("Subscription {} cancelled via Unsubscribe", subscription_id);
            break;
        }

        // Generate timestamp in "secs.nanosecs" format
        let duration = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let timestamp = format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos());

        let event = Event {
            subscription_id: subscription_id.clone(),
            sequence,
            data: json_value,
            timestamp,
            dropped: None, // TODO: Implement dropped event tracking
        };

        if event_tx.send(event).is_err() {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Event channel closed, terminating subscription: {}",
                subscription_id
            );
            break;
        }

        sequence += 1;
    }

    #[cfg(feature = "tracing")]
    tracing::debug!("Subscription future terminated: {}", subscription_id);
}

/// Handles record.unsubscribe method
///
/// Cancels an active subscription.
///
/// # Arguments
/// * `conn_state` - Connection state (for subscription tracking)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "subscription_id" field)
///
/// # Returns
/// Success response, or error if subscription not found
#[cfg(feature = "std")]
async fn handle_record_unsubscribe(
    conn_state: &mut ConnectionState,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response {
    // Parse subscription_id parameter
    let subscription_id = match params {
        Some(serde_json::Value::Object(ref map)) => match map.get("subscription_id") {
            Some(serde_json::Value::String(id)) => id.clone(),
            _ => {
                return Response::error(
                    request_id,
                    "invalid_params",
                    "Missing or invalid 'subscription_id' parameter".to_string(),
                )
            }
        },
        _ => {
            return Response::error(
                request_id,
                "invalid_params",
                "Missing 'subscription_id' parameter".to_string(),
            )
        }
    };

    #[cfg(feature = "tracing")]
    tracing::debug!("Unsubscribing from subscription_id: {}", subscription_id);

    // Look up and remove the subscription. Setting the cancel flag tells
    // the per-subscription future to exit on its next poll; the future
    // itself is reaped from `subs` by the connection's outer drain loop.
    match conn_state.subscriptions.remove(&subscription_id) {
        Some(cancel) => {
            cancel.store(true, Ordering::Relaxed);

            #[cfg(feature = "tracing")]
            tracing::debug!("Cancelled subscription {}", subscription_id);

            Response::success(
                request_id,
                serde_json::json!({
                    "subscription_id": subscription_id,
                    "status": "cancelled"
                }),
            )
        }
        None => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Subscription not found: {}", subscription_id);

            Response::error(
                request_id,
                "not_found",
                format!("Subscription '{}' not found", subscription_id),
            )
        }
    }
}

/// Handles record.drain method
///
/// Drains all pending values from a record's drain reader. On the first call for
/// a given record, creates a dedicated drain reader (returns empty). Subsequent
/// calls return all values accumulated since the previous drain.
///
/// # Arguments
/// * `db` - Database instance
/// * `conn_state` - Connection state (for drain reader management)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "name" field, optional "limit")
///
/// # Returns
/// Success response with `record_name`, `values` array, and `count`, or error if:
/// - Missing/invalid parameters
/// - Record not found
/// - Record not configured with `.with_remote_access()`
#[cfg(feature = "std")]
async fn handle_record_drain<R>(
    db: &Arc<AimDb<R>>,
    conn_state: &mut ConnectionState,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    // Extract record name from params
    let record_name = match params {
        Some(serde_json::Value::Object(ref map)) => match map.get("name") {
            Some(serde_json::Value::String(name)) => name.clone(),
            _ => {
                return Response::error(
                    request_id,
                    "invalid_params",
                    "Missing or invalid 'name' parameter (expected string)".to_string(),
                );
            }
        },
        _ => {
            return Response::error(
                request_id,
                "invalid_params",
                "Missing params object".to_string(),
            );
        }
    };

    // Optional: limit parameter
    // Use try_from instead of `as` to avoid silent truncation on 32-bit targets
    // (values that don't fit in usize are treated as "no limit").
    let limit = params
        .as_ref()
        .and_then(|p| p.as_object())
        .and_then(|map| map.get("limit"))
        .and_then(|v| v.as_u64())
        .map(|v| usize::try_from(v).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Draining record: {} (limit: {})",
        record_name,
        if limit == usize::MAX {
            "all".to_string()
        } else {
            limit.to_string()
        }
    );

    // Lazily create drain reader on first call for this record
    if !conn_state.drain_readers.contains_key(&record_name) {
        // Resolve record key → RecordId → AnyRecord → subscribe_json()
        let id = match db.inner().resolve_str(&record_name) {
            Some(id) => id,
            None => {
                return Response::error(
                    request_id,
                    "not_found",
                    format!("Record '{}' not found", record_name),
                );
            }
        };

        let record = match db.inner().storage(id) {
            Some(r) => r,
            None => {
                return Response::error(
                    request_id,
                    "not_found",
                    format!("Record '{}' storage not found", record_name),
                );
            }
        };

        let reader = match record.subscribe_json() {
            Ok(r) => r,
            Err(e) => {
                return Response::error(
                    request_id,
                    "remote_access_not_enabled",
                    format!(
                        "Record '{}' not configured with .with_remote_access(): {}",
                        record_name, e
                    ),
                );
            }
        };

        conn_state.drain_readers.insert(record_name.clone(), reader);
    }

    // Drain all pending values from the reader
    let reader = conn_state.drain_readers.get_mut(&record_name).unwrap();
    let mut values = Vec::new();

    loop {
        if values.len() >= limit {
            break;
        }
        match reader.try_recv_json() {
            Ok(val) => values.push(val),
            Err(DbError::BufferEmpty) => break,
            Err(DbError::BufferLagged { .. }) => {
                // Ring overflowed since last drain — cursor resets.
                // Log warning, keep draining.
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    "Drain reader lagged for record '{}' — some values were lost",
                    record_name
                );
                continue;
            }
            Err(_) => break,
        }
    }

    let count = values.len();

    #[cfg(feature = "tracing")]
    tracing::debug!("Drained {} values from record '{}'", count, record_name);

    Response::success(
        request_id,
        json!({
            "record_name": record_name,
            "values": values,
            "count": count,
        }),
    )
}

// ============================================================================
// Persistence Query (record.query)
// ============================================================================

/// Type-erased query handler registered by `aimdb-persistence` via Extensions.
///
/// This keeps `aimdb-core` free of persistence-specific imports. The handler is
/// a boxed async function that accepts query parameters (record pattern, limit,
/// start/end timestamps) and returns a JSON value with the results.
///
/// Registered by `aimdb_persistence` via the `with_persistence()` builder extension.
pub type QueryHandlerFn = Box<
    dyn Fn(
            QueryHandlerParams,
        ) -> core::pin::Pin<
            Box<dyn core::future::Future<Output = Result<serde_json::Value, String>> + Send>,
        > + Send
        + Sync,
>;

/// Parameters for the type-erased query handler.
#[derive(Debug, Clone)]
pub struct QueryHandlerParams {
    /// Record pattern (supports `*` wildcard).
    pub name: String,
    /// Maximum results per matching record.
    pub limit: Option<usize>,
    /// Optional start timestamp (Unix ms).
    pub start: Option<u64>,
    /// Optional end timestamp (Unix ms).
    pub end: Option<u64>,
}

/// Handles `record.query` method.
///
/// Delegates to a [`QueryHandlerFn`] stored in the database's `Extensions`
/// TypeMap. If no handler is registered (i.e. persistence is not configured),
/// returns an error.
#[cfg(feature = "std")]
async fn handle_record_query<R>(
    db: &Arc<AimDb<R>>,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    // Extract the query handler from Extensions.
    let handler = match db.extensions().get::<QueryHandlerFn>() {
        Some(h) => h,
        None => {
            return Response::error(
                request_id,
                "not_configured",
                "Persistence not configured. Call .with_persistence() on the builder.".to_string(),
            );
        }
    };

    // Parse parameters
    let (name, limit, start, end) = match &params {
        Some(serde_json::Value::Object(map)) => {
            let name = map
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("*")
                .to_string();
            let limit = map
                .get("limit")
                .and_then(|v| v.as_u64())
                .and_then(|v| usize::try_from(v).ok());
            let start = map.get("start").and_then(|v| v.as_u64());
            let end = map.get("end").and_then(|v| v.as_u64());
            (name, limit, start, end)
        }
        _ => ("*".to_string(), None, None, None),
    };

    let query_params = QueryHandlerParams {
        name,
        limit,
        start,
        end,
    };

    match handler(query_params).await {
        Ok(result) => Response::success(request_id, result),
        Err(msg) => Response::error(request_id, "query_error", msg),
    }
}

// ============================================================================
// Graph Introspection Methods
// ============================================================================

/// Handles graph.nodes method
///
/// Returns all nodes in the dependency graph with their metadata.
/// Each node represents a record with its origin, buffer type, and connections.
///
/// # Arguments
/// * `db` - Database instance
/// * `request_id` - Request ID for the response
///
/// # Returns
/// Success response with array of GraphNode objects:
/// - `key`: Record key (e.g., "temp.vienna")
/// - `origin`: How the record gets its values (source, link, transform, passive)
/// - `buffer_type`: Buffer type ("spmc_ring", "single_latest", "mailbox", "none")
/// - `buffer_capacity`: Optional buffer capacity
/// - `tap_count`: Number of taps attached
/// - `has_outbound_link`: Whether an outbound connector is configured
#[cfg(feature = "std")]
async fn handle_graph_nodes<R>(db: &Arc<AimDb<R>>, request_id: u64) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!("Getting dependency graph nodes");

    let graph = db.inner().dependency_graph();
    let nodes = &graph.nodes;

    #[cfg(feature = "tracing")]
    tracing::debug!("Returning {} graph nodes", nodes.len());

    Response::success(request_id, json!(nodes))
}

/// Handles graph.edges method
///
/// Returns all edges in the dependency graph representing data flow between records.
/// Edges are directed from source to target and include the edge type.
///
/// # Arguments
/// * `db` - Database instance
/// * `request_id` - Request ID for the response
///
/// # Returns
/// Success response with array of GraphEdge objects:
/// - `from`: Source record key
/// - `to`: Target record key
/// - `edge_type`: Type of connection (TransformInput, TransformJoinInput, etc.)
#[cfg(feature = "std")]
async fn handle_graph_edges<R>(db: &Arc<AimDb<R>>, request_id: u64) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!("Getting dependency graph edges");

    let graph = db.inner().dependency_graph();
    let edges = &graph.edges;

    #[cfg(feature = "tracing")]
    tracing::debug!("Returning {} graph edges", edges.len());

    Response::success(request_id, json!(edges))
}

/// Handles graph.topo_order method
///
/// Returns the topological ordering of records in the dependency graph.
/// This ordering ensures that all dependencies are processed before dependents.
/// Used for spawn ordering and understanding data flow.
///
/// # Arguments
/// * `db` - Database instance
/// * `request_id` - Request ID for the response
///
/// # Returns
/// Success response with array of record keys in topological order:
/// - Sources and passive records first
/// - Transform outputs after their inputs
/// - Respects the DAG structure for proper initialization order
#[cfg(feature = "std")]
async fn handle_graph_topo_order<R>(db: &Arc<AimDb<R>>, request_id: u64) -> Response
where
    R: crate::RuntimeAdapter + 'static,
{
    #[cfg(feature = "tracing")]
    tracing::debug!("Getting topological order");

    let graph = db.inner().dependency_graph();
    let topo_order = graph.topo_order();

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Returning topological order with {} records",
        topo_order.len()
    );

    Response::success(request_id, json!(topo_order))
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use futures_core::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};

    /// A `Stream<Item = serde_json::Value>` that never yields a value but
    /// flips a flag when dropped. Used to verify that dropping the
    /// per-connection `FuturesUnordered` drops the per-subscription
    /// future, which in turn drops its underlying record stream — the
    /// invariant the AimX spawn-free refactor depends on for cancellation
    /// on connection close.
    struct DropTracker {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl Stream for DropTracker {
        type Item = serde_json::Value;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            // Park forever; we only care that the stream gets dropped.
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn dropping_subs_set_drops_subscription_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let stream = DropTracker {
            dropped: dropped.clone(),
        };

        let (event_tx, _event_rx) = mpsc::unbounded_channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));

        let mut subs: FuturesUnordered<BoxFuture> = FuturesUnordered::new();
        subs.push(Box::pin(run_subscription(
            stream,
            "sub-1".to_string(),
            event_tx,
            cancel,
        )));

        // Drive the set once so the future is actually pinned/installed.
        tokio::task::yield_now().await;
        let _ = futures_util::future::poll_fn(|cx| {
            let _ = Pin::new(&mut subs).poll_next(cx);
            Poll::Ready(())
        })
        .await;

        assert!(
            !dropped.load(Ordering::SeqCst),
            "drop must not have fired yet"
        );

        // Dropping the set drops every contained future, which in turn
        // drops the stream owned by `run_subscription`.
        drop(subs);

        assert!(
            dropped.load(Ordering::SeqCst),
            "dropping the FuturesUnordered must drop the subscription stream"
        );
    }

    #[tokio::test]
    async fn unsubscribe_flag_terminates_subscription_on_next_event() {
        use futures_util::stream::unfold;

        // Channel-backed stream so we control exactly when the future
        // advances past each `stream.next().await`. Without this, a
        // synchronous source like `stream::iter` would race past the
        // flag-flip before the test can observe it.
        let (val_tx, val_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let values = unfold(
            val_rx,
            |mut rx| async move { rx.recv().await.map(|v| (v, rx)) },
        );

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));

        let cancel_for_future = cancel.clone();
        let handle = tokio::spawn(run_subscription(
            values,
            "sub-1".to_string(),
            event_tx,
            cancel_for_future,
        ));

        // Feed one value and confirm it propagates as an Event.
        val_tx.send(serde_json::json!({"v": 1})).unwrap();
        let event = event_rx.recv().await.expect("expected one event");
        assert_eq!(event.subscription_id, "sub-1");

        // Flip the cancel flag, then feed a second value to wake the
        // stream. The future must observe the flag *after* the next
        // `stream.next().await` returns and exit before sending the
        // second Event.
        cancel.store(true, Ordering::Relaxed);
        val_tx.send(serde_json::json!({"v": 2})).unwrap();

        // The future must complete on its own — no abort.
        handle
            .await
            .expect("subscription future should exit cleanly after cancel flag");

        // And the second value must not have produced an Event.
        assert!(
            event_rx.try_recv().is_err(),
            "no further events should be sent after cancel"
        );
    }

    #[tokio::test]
    async fn dropping_subs_set_drops_inner_stream_state() {
        // Stronger integration-style check: a real channel-backed stream
        // (the same shape `stream_record_updates` returns via `unfold`)
        // is held inside a `run_subscription` future, which is held by a
        // `FuturesUnordered`. Dropping the set must drop the channel's
        // receiver, which we observe by `val_tx.send(...)` failing.
        use futures_util::stream::unfold;

        let (val_tx, val_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let values = unfold(
            val_rx,
            |mut rx| async move { rx.recv().await.map(|v| (v, rx)) },
        );

        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
        let cancel = Arc::new(AtomicBool::new(false));

        let mut subs: FuturesUnordered<BoxFuture> = FuturesUnordered::new();
        subs.push(Box::pin(run_subscription(
            values,
            "sub-1".to_string(),
            event_tx,
            cancel,
        )));

        // Drive the set until the subscription is observably alive.
        val_tx.send(serde_json::json!({"v": 1})).unwrap();
        tokio::select! {
            event = event_rx.recv() => {
                assert_eq!(event.unwrap().subscription_id, "sub-1");
            }
            _ = subs.next() => panic!("subscription future ended unexpectedly"),
        }

        // Connection going away: drop the whole set. This must drop the
        // boxed future, which drops the stream, which drops `val_rx`.
        drop(subs);

        assert!(
            val_tx.send(serde_json::json!({"v": 2})).is_err(),
            "after dropping the FuturesUnordered, the inner stream's \
             receiver must be dropped — `send` is the observable proxy"
        );
    }
}
