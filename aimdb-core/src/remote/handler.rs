//! Connection handler for AimX protocol
//!
//! Handles individual client connections, including handshake, authentication,
//! and protocol method dispatch.
//!
//! # Architecture: Event Funnel Pattern
//!
//! Subscriptions use a funnel pattern for clean event delivery:
//! - Each subscription spawns a consumer task that reads from the record buffer
//! - Consumer tasks send events to a shared mpsc channel (the "funnel")
//! - A single writer task drains the funnel and writes events to the UnixStream
//! - This ensures NDJSON line integrity and prevents write interleaving

use crate::remote::{
    AimxConfig, Event, HelloMessage, RecordMetadata, Request, Response, WelcomeMessage,
};
use crate::{AimDb, DbError, DbResult};

#[cfg(feature = "std")]
use std::collections::HashMap;
#[cfg(feature = "std")]
use std::sync::Arc;

#[cfg(feature = "std")]
use serde_json::json;
#[cfg(feature = "std")]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(feature = "std")]
use tokio::net::UnixStream;
#[cfg(feature = "std")]
use tokio::sync::mpsc;
#[cfg(feature = "std")]
use tokio::sync::oneshot;

/// Handle for an active subscription
///
/// Tracks the state needed to manage a single subscription's lifecycle.
#[cfg(feature = "std")]
#[allow(dead_code)] // record_name used only in tracing feature
struct SubscriptionHandle {
    /// Unique subscription identifier (returned to client)
    subscription_id: String,

    /// Record name being subscribed to
    record_name: String,

    /// Signal to cancel this subscription
    /// When sent, the consumer task will terminate
    cancel_tx: oneshot::Sender<()>,
}

/// Connection state for managing subscriptions
///
/// Tracks all active subscriptions for a single client connection.
#[cfg(feature = "std")]
struct ConnectionState {
    /// Active subscriptions by subscription_id
    subscriptions: HashMap<String, SubscriptionHandle>,

    /// Counter for generating unique subscription IDs
    next_subscription_id: u64,

    /// Event funnel: all subscription tasks send events here
    /// This channel feeds the single writer task
    event_tx: mpsc::UnboundedSender<Event>,
}

#[cfg(feature = "std")]
impl ConnectionState {
    /// Creates a new connection state
    fn new(event_tx: mpsc::UnboundedSender<Event>) -> Self {
        Self {
            subscriptions: HashMap::new(),
            next_subscription_id: 1,
            event_tx,
        }
    }

    /// Generates a unique subscription ID for this connection
    fn generate_subscription_id(&mut self) -> String {
        let id = format!("sub-{}", self.next_subscription_id);
        self.next_subscription_id += 1;
        id
    }

    /// Adds a subscription to the connection state
    fn add_subscription(&mut self, handle: SubscriptionHandle) {
        self.subscriptions
            .insert(handle.subscription_id.clone(), handle);
    }

    /// Removes and returns a subscription by ID
    #[allow(dead_code)]
    fn remove_subscription(&mut self, subscription_id: &str) -> Option<SubscriptionHandle> {
        self.subscriptions.remove(subscription_id)
    }

    /// Cancels all active subscriptions
    ///
    /// Sends cancel signals to all subscription tasks and clears the map.
    /// Called when the client disconnects.
    async fn cancel_all_subscriptions(&mut self) {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "Canceling {} active subscriptions",
            self.subscriptions.len()
        );

        for (_id, handle) in self.subscriptions.drain() {
            // Send cancel signal (ignore if receiver already dropped)
            let _ = handle.cancel_tx.send(());
        }
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
/// ┌─────────────────┐
/// │ Subscription 1  │───┐
/// │ Consumer Task   │   │
/// └─────────────────┘   │
///                       ├──► Event Funnel ───► select! loop ───► UnixStream
/// ┌─────────────────┐   │     (mpsc)          (interleaved    
/// │ Subscription 2  │───┘                      writes)
/// │ Consumer Task   │
/// └─────────────────┘
/// ```
///
/// The main loop uses `tokio::select!` to interleave:
/// - Reading requests from the stream
/// - Writing events from subscriptions
///
/// This ensures both responses and events are written without blocking.
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

    // Create event funnel: all subscription tasks will send events here
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();

    // Initialize connection state
    let mut conn_state = ConnectionState::new(event_tx);

    // Main loop: interleave reading requests and writing events
    loop {
        let mut line = String::new();

        tokio::select! {
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
                        let response = handle_request(&db, &config, &mut conn_state, request).await;

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
        }
    }

    // Cleanup: cancel all active subscriptions
    conn_state.cancel_all_subscriptions().await;

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
    request: Request,
) -> Response
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
        "record.set" => handle_record_set(db, config, request.id, request.params).await,
        "record.subscribe" => {
            handle_record_subscribe(db, config, conn_state, request.id, request.params).await
        }
        "record.unsubscribe" => {
            handle_record_unsubscribe(conn_state, request.id, request.params).await
        }
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
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
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

    // Find the record's TypeId by name
    let type_id_opt = db.inner().records.iter().find_map(|(tid, record)| {
        let metadata = record.collect_metadata(*tid);
        if metadata.name == record_name {
            Some(*tid)
        } else {
            None
        }
    });

    let type_id = match type_id_opt {
        Some(tid) => tid,
        None => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Record not found: {}", record_name);

            return Response::error(
                request_id,
                "not_found",
                format!("Record '{}' not found", record_name),
            );
        }
    };

    // Check if record is in the writable_records set
    if !writable_records.contains(&type_id) {
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
/// Subscribes to real-time updates for a record.
///
/// # Arguments
/// * `db` - Database instance
/// * `config` - Remote access configuration
/// * `conn_state` - Connection state (for subscription tracking)
/// * `request_id` - Request ID for the response
/// * `params` - Request parameters (must contain "name" field with record name)
///
/// # Returns
/// Success response with subscription_id and queue_size, or error if:
/// - Missing/invalid parameters
/// - Record not found
/// - Too many subscriptions
#[cfg(feature = "std")]
async fn handle_record_subscribe<R>(
    db: &Arc<AimDb<R>>,
    config: &AimxConfig,
    conn_state: &mut ConnectionState,
    request_id: u64,
    params: Option<serde_json::Value>,
) -> Response
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
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

    // Find the record's TypeId by name
    // We need to search through the database's records to find the matching TypeId
    let type_id_opt = db.inner().records.iter().find_map(|(tid, record)| {
        let metadata = record.collect_metadata(*tid);
        if metadata.name == record_name {
            Some(*tid)
        } else {
            None
        }
    });

    let type_id = match type_id_opt {
        Some(tid) => tid,
        None => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Record not found: {}", record_name);

            return Response::error(
                request_id,
                "not_found",
                format!("Record '{}' not found", record_name),
            );
        }
    };

    // Check max subscriptions limit
    if conn_state.subscriptions.len() >= config.subscription_queue_size {
        #[cfg(feature = "tracing")]
        tracing::warn!(
            "Too many subscriptions: {} (max: {})",
            conn_state.subscriptions.len(),
            config.subscription_queue_size
        );

        return Response::error(
            request_id,
            "too_many_subscriptions",
            format!(
                "Maximum subscriptions reached: {}",
                config.subscription_queue_size
            ),
        );
    }

    // Generate unique subscription ID
    let subscription_id = conn_state.generate_subscription_id();

    // Subscribe to record updates via the database API
    let (value_rx, cancel_tx) =
        match db.subscribe_record_updates(type_id, config.subscription_queue_size) {
            Ok(channels) => channels,
            Err(e) => {
                #[cfg(feature = "tracing")]
                tracing::error!("Failed to subscribe to record updates: {}", e);

                return Response::error(
                    request_id,
                    "internal_error",
                    format!("Failed to subscribe: {}", e),
                );
            }
        };

    // Spawn event streaming task for this subscription
    let event_tx = conn_state.event_tx.clone();
    let sub_id_clone = subscription_id.clone();
    let stream_handle = tokio::spawn(async move {
        stream_subscription_events(sub_id_clone, value_rx, event_tx).await;
    });

    // Store subscription handle
    let handle = SubscriptionHandle {
        subscription_id: subscription_id.clone(),
        record_name: record_name.clone(),
        cancel_tx,
    };
    conn_state.add_subscription(handle);

    // Detach the streaming task (it will run until cancelled or channel closes)
    std::mem::drop(stream_handle);

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
            "queue_size": config.subscription_queue_size,
        }),
    )
}

/// Streams subscription events from value channel to event channel
///
/// Reads JSON values from the subscription's receiver and converts them
/// into Event messages with sequence numbers and timestamps.
///
/// # Arguments
/// * `subscription_id` - Unique subscription identifier
/// * `value_rx` - Receiver for JSON values from the database
/// * `event_tx` - Sender for Event messages to the client
#[cfg(feature = "std")]
async fn stream_subscription_events(
    subscription_id: String,
    mut value_rx: tokio::sync::mpsc::Receiver<serde_json::Value>,
    event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
) {
    let mut sequence: u64 = 1;

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Event streaming task started for subscription: {}",
        subscription_id
    );

    while let Some(json_value) = value_rx.recv().await {
        // Generate timestamp in "secs.nanosecs" format
        let timestamp = format!(
            "{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
        );

        // Create event
        let event = Event {
            subscription_id: subscription_id.clone(),
            sequence,
            data: json_value,
            timestamp,
            dropped: None, // TODO: Implement dropped event tracking
        };

        // Send event to the funnel
        if event_tx.send(event).is_err() {
            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Event channel closed, terminating stream for subscription: {}",
                subscription_id
            );
            break;
        }

        sequence += 1;
    }

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Event streaming task terminated for subscription: {}",
        subscription_id
    );
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

    // Look up and remove the subscription
    match conn_state.subscriptions.remove(&subscription_id) {
        Some(handle) => {
            // Send cancellation signal to the streaming task
            // It's okay if this fails (task may have already terminated)
            let _ = handle.cancel_tx.send(());

            #[cfg(feature = "tracing")]
            tracing::debug!(
                "Cancelled subscription {} for record {}",
                subscription_id,
                handle.record_name
            );

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
