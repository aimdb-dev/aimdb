//! Stage profiling tools.
//!
//! `get_stage_profiling` reports per-stage timing (`.source()`/`.tap()`/`.link()`
//! callback wall-clock time) for records matching a key, and flags the slowest
//! stage. `reset_stage_profiling` clears the counters on the server.
//!
//! These require the target AimDB instance to be built with the `profiling`
//! feature; without it, records simply carry no `stage_profiling` data.

use crate::error::{McpError, McpResult};
use aimdb_client::AimxConnection;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

#[derive(Debug, Deserialize)]
struct GetStageProfilingParams {
    endpoint: Option<String>,
    /// Substring matched against record name/key (e.g. `"Temperature"`).
    record_key: String,
}

#[derive(Debug, Deserialize)]
struct ResetStageProfilingParams {
    endpoint: Option<String>,
}

async fn connect(endpoint: &str) -> McpResult<AimxConnection> {
    if let Some(pool) = super::connection_pool() {
        pool.get_connection(endpoint)
            .await
            .map_err(McpError::Client)
    } else {
        AimxConnection::connect(endpoint)
            .await
            .map_err(McpError::Client)
    }
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

/// Returns per-stage profiling metrics for records matching `record_key`, plus a
/// `bottleneck` (the stage with the highest average wall-clock time).
pub async fn get_stage_profiling(args: Option<Value>) -> McpResult<Value> {
    debug!("get_stage_profiling called");
    let params: GetStageProfilingParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("get_stage_profiling: {e}")))?;
    let endpoint = super::resolve_endpoint(params.endpoint)?;

    let client = connect(&endpoint).await?;
    let records = client.list_records().await.map_err(McpError::Client)?;

    let mut out = Vec::new();
    for rec in records {
        if !(rec.name.contains(&params.record_key) || rec.record_key.contains(&params.record_key)) {
            continue;
        }
        let stages = rec.stage_profiling.clone().unwrap_or_default();

        // Bottleneck = stage with the highest avg_time_ns among those actually invoked.
        let bottleneck = stages
            .iter()
            .filter(|s| s.call_count > 0)
            .max_by_key(|s| s.avg_time_ns)
            .map(|s| {
                let label = s.name.clone().unwrap_or_else(|| format!("{}[{}]", s.stage_type, s.index));
                json!({
                    "stage_type": s.stage_type,
                    "index": s.index,
                    "name": s.name,
                    "avg_time_ns": s.avg_time_ns,
                    "call_count": s.call_count,
                    "recommendation": format!(
                        "{} '{}' averages {:.2} ms per call ({} calls) — this is the slowest stage.",
                        s.stage_type, label, ms(s.avg_time_ns), s.call_count
                    ),
                })
            });

        out.push(json!({
            "record": rec.name,
            "key": rec.record_key,
            "profiling_enabled": rec.stage_profiling.is_some(),
            "stages": serde_json::to_value(&stages)?,
            "bottleneck": bottleneck,
        }));
    }

    if out.is_empty() {
        return Ok(json!({
            "found": false,
            "record_key": params.record_key,
            "message": "No records matching this key were found.",
        }));
    }

    Ok(json!({
        "found": true,
        "record_key": params.record_key,
        "records": out,
    }))
}

/// Resets stage profiling counters for every record on the target instance.
pub async fn reset_stage_profiling(args: Option<Value>) -> McpResult<Value> {
    debug!("reset_stage_profiling called");
    let params: ResetStageProfilingParams = serde_json::from_value(args.unwrap_or(Value::Null))
        .map_err(|e| McpError::InvalidParams(format!("reset_stage_profiling: {e}")))?;
    let endpoint = super::resolve_endpoint(params.endpoint)?;

    let client = connect(&endpoint).await?;
    match client.reset_stage_profiling().await {
        Ok(_) => Ok(json!({
            "reset": true,
            "message": "Stage profiling counters reset on all records.",
        })),
        Err(aimdb_client::ClientError::ServerError { ref code, .. })
            if code == "method_not_found" =>
        {
            // The server was built without the `profiling` feature.
            Ok(json!({
                "reset": false,
                "message": "The target instance does not support profiling.reset (built without the `profiling` feature?).",
            }))
        }
        Err(e) => Err(McpError::Client(e)),
    }
}
