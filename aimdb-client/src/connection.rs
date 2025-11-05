//! AimX Client Connection
//!
//! Async client for connecting to AimDB instances via Unix domain sockets.

use crate::error::{ClientError, ClientResult};
use crate::protocol::{
    cli_hello, parse_message, serialize_message, Event, EventMessage, RecordMetadata, Request,
    RequestExt, Response, ResponseExt, WelcomeMessage,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// Timeout for connection operations
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// AimX protocol client
pub struct AimxClient {
    socket_path: PathBuf,
    stream: OwnedWriteHalf,
    reader: BufReader<OwnedReadHalf>,
    request_id_counter: u64,
    server_info: WelcomeMessage,
}

impl AimxClient {
    /// Connect to an AimDB instance
    pub async fn connect(socket_path: impl AsRef<Path>) -> ClientResult<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();

        // Connect with timeout
        let stream = tokio::time::timeout(CONNECTION_TIMEOUT, UnixStream::connect(&socket_path))
            .await
            .map_err(|_| {
                ClientError::connection_failed(
                    socket_path.display().to_string(),
                    "connection timeout",
                )
            })?
            .map_err(|e| {
                ClientError::connection_failed(socket_path.display().to_string(), e.to_string())
            })?;

        // Split into reader and writer
        let (reader_stream, writer_stream) = stream.into_split();

        let reader = BufReader::new(reader_stream);
        let mut client = Self {
            socket_path,
            stream: writer_stream,
            reader,
            request_id_counter: 0,
            server_info: WelcomeMessage {
                version: String::new(),
                server: String::new(),
                permissions: Vec::new(),
                writable_records: Vec::new(),
                max_subscriptions: None,
                authenticated: None,
            },
        };

        // Perform handshake
        client.handshake().await?;

        Ok(client)
    }

    /// Perform protocol handshake
    async fn handshake(&mut self) -> ClientResult<()> {
        // Send Hello
        let hello = cli_hello();
        self.write_message(&hello).await?;

        // Receive Welcome
        let welcome: WelcomeMessage = self.read_message().await?;
        self.server_info = welcome;

        Ok(())
    }

    /// Get server information
    pub fn server_info(&self) -> &WelcomeMessage {
        &self.server_info
    }

    /// Send a request and wait for response
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> ClientResult<serde_json::Value> {
        self.request_id_counter += 1;
        let id = self.request_id_counter;

        let request = if let Some(params) = params {
            Request::with_params(id, method, params)
        } else {
            Request::new(id, method)
        };

        self.write_message(&request).await?;

        let response: Response = self.read_message().await?;

        match response.into_result() {
            Ok(result) => Ok(result),
            Err(error) => Err(ClientError::server_error(
                error.code,
                error.message,
                error.details,
            )),
        }
    }

    /// List all registered records
    pub async fn list_records(&mut self) -> ClientResult<Vec<RecordMetadata>> {
        let result = self.send_request("record.list", None).await?;
        let records: Vec<RecordMetadata> = serde_json::from_value(result)?;
        Ok(records)
    }

    /// Get current value of a record
    pub async fn get_record(&mut self, name: &str) -> ClientResult<serde_json::Value> {
        let params = json!({ "record": name });
        self.send_request("record.get", Some(params)).await
    }

    /// Set value of a writable record
    pub async fn set_record(
        &mut self,
        name: &str,
        value: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        let params = json!({
            "name": name,
            "value": value
        });
        self.send_request("record.set", Some(params)).await
    }

    /// Subscribe to record updates
    pub async fn subscribe(&mut self, name: &str, queue_size: usize) -> ClientResult<String> {
        let params = json!({
            "name": name,
            "queue_size": queue_size
        });
        let result = self.send_request("record.subscribe", Some(params)).await?;

        let subscription_id = result["subscription_id"]
            .as_str()
            .ok_or_else(|| {
                ClientError::Other(anyhow::anyhow!("Missing subscription_id in response"))
            })?
            .to_string();

        Ok(subscription_id)
    }

    /// Unsubscribe from record updates
    pub async fn unsubscribe(&mut self, subscription_id: &str) -> ClientResult<()> {
        let params = json!({ "subscription_id": subscription_id });
        self.send_request("record.unsubscribe", Some(params))
            .await?;
        Ok(())
    }

    /// Receive next event from subscription
    pub async fn receive_event(&mut self) -> ClientResult<Event> {
        let event_msg: EventMessage = self.read_message().await?;
        Ok(event_msg.event)
    }

    /// Write a message to the stream
    async fn write_message<T: serde::Serialize>(&mut self, msg: &T) -> ClientResult<()> {
        let data = serialize_message(msg)?;
        self.stream.write_all(data.as_bytes()).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Read a message from the stream
    async fn read_message<T: for<'de> serde::Deserialize<'de>>(&mut self) -> ClientResult<T> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        if line.is_empty() {
            return Err(ClientError::connection_failed(
                self.socket_path.display().to_string(),
                "connection closed by server",
            ));
        }

        parse_message(&line).map_err(|e| e.into())
    }
}
