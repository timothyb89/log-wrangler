use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;

use color_eyre::eyre::eyre;
use color_eyre::Result;
use futures::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::tungstenite;

use super::{RawLog, SourceMessage};

/// Parameters for a Loki query, sent via the watch channel to restart the source.
#[derive(Clone, Debug)]
pub struct LokiSourceParams {
    pub query: String,
    pub start_ns: i128,
    pub end_ns: Option<i128>,
    pub follow: bool,
}

const BATCH_LIMIT: usize = 5000;

// --- Loki API response types ---

#[derive(Deserialize)]
struct LokiResponse {
    #[allow(dead_code)]
    status: String,
    data: LokiData,
}

#[derive(Deserialize)]
struct LokiData {
    #[allow(dead_code)]
    #[serde(rename = "resultType")]
    result_type: String,
    result: Vec<LokiStream>,
}

#[derive(Deserialize)]
struct LokiStream {
    stream: HashMap<String, String>,
    values: Vec<(String, String)>,
}

/// WebSocket tail frame from `/loki/api/v1/tail`.
#[derive(Deserialize)]
struct LokiTailResponse {
    streams: Vec<LokiStream>,
    #[allow(dead_code)]
    dropped_entries: Option<Vec<serde_json::Value>>,
}

// --- Conversion helpers ---

fn parse_ns_timestamp(ns_str: &str) -> Result<jiff::Zoned> {
    let ns: i128 = ns_str
        .parse()
        .map_err(|e| eyre!("Invalid nanosecond timestamp '{}': {}", ns_str, e))?;
    let ts = jiff::Timestamp::from_nanosecond(ns)
        .map_err(|e| eyre!("Timestamp out of range: {}", e))?;
    Ok(ts.to_zoned(jiff::tz::TimeZone::UTC))
}

fn stream_to_raw_logs(stream: LokiStream, source_id: u16) -> Vec<(i128, RawLog)> {
    let labels = stream.stream;
    stream
        .values
        .into_iter()
        .filter_map(|(ts_str, line)| {
            let ns: i128 = ts_str.parse().ok()?;
            let timestamp = parse_ns_timestamp(&ts_str).ok()?;
            Some((ns, RawLog {
                timestamp,
                message: line,
                labels: labels.clone(),
                source_id,
            }))
        })
        .collect()
}

// --- HTTP range query ---

async fn fetch_range(
    client: &reqwest::Client,
    base_url: &url::Url,
    query: &str,
    start_ns: i128,
    end_ns: i128,
    limit: usize,
) -> Result<Vec<LokiStream>> {
    let url = format!(
        "{}/loki/api/v1/query_range",
        base_url.as_str().trim_end_matches('/')
    );

    let resp = client
        .get(&url)
        .query(&[
            ("query", query),
            ("start", &start_ns.to_string()),
            ("end", &end_ns.to_string()),
            ("limit", &limit.to_string()),
            ("direction", "forward"),
        ])
        .send()
        .await
        .map_err(|e| eyre!("Loki query_range request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(eyre!(
            "Loki query_range returned {}: {}",
            status,
            body.chars().take(500).collect::<String>()
        ));
    }

    let loki_resp: LokiResponse = resp
        .json()
        .await
        .map_err(|e| eyre!("Failed to parse Loki response: {}", e))?;

    Ok(loki_resp.data.result)
}

/// Fetch all logs in a time range with pagination. Returns the last timestamp
/// seen (nanoseconds), or `start_ns` if no logs were returned.
async fn fetch_all(
    client: &reqwest::Client,
    base_url: &url::Url,
    query: &str,
    start_ns: i128,
    end_ns: i128,
    tx: &mpsc::Sender<SourceMessage>,
    source_id: u16,
) -> Result<i128> {
    let mut cursor = start_ns;
    let mut last_ts = start_ns;

    loop {
        let streams = fetch_range(client, base_url, query, cursor, end_ns, BATCH_LIMIT).await?;

        let mut total_entries = 0;
        let mut batch_last_ts = cursor;

        for stream in streams {
            let logs = stream_to_raw_logs(stream, source_id);
            total_entries += logs.len();
            for (ns, raw_log) in logs {
                if ns > batch_last_ts {
                    batch_last_ts = ns;
                }
                if ns > last_ts {
                    last_ts = ns;
                }
                if tx.send(SourceMessage::Log(raw_log)).is_err() {
                    return Ok(last_ts);
                }
            }
        }

        // If we got fewer than the limit, we've fetched everything.
        if total_entries < BATCH_LIMIT {
            break;
        }

        // Paginate: move cursor past the last timestamp we saw.
        cursor = batch_last_ts + 1;
    }

    Ok(last_ts)
}

// --- WebSocket tail ---

/// Convert an HTTP(S) URL to a WS(S) URL for the tail endpoint.
fn make_tail_url(base_url: &url::Url, query: &str, start_ns: i128) -> Result<String> {
    let scheme = match base_url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => return Err(eyre!("Unsupported URL scheme '{}' for WebSocket", other)),
    };

    let host = base_url
        .host_str()
        .ok_or_else(|| eyre!("Missing host in base URL"))?;

    let port_part = match base_url.port() {
        Some(p) => format!(":{}", p),
        None => String::new(),
    };

    let path = base_url.path().trim_end_matches('/');

    // Build the URL with properly encoded query params.
    let base = format!(
        "{}://{}{}{}/loki/api/v1/tail",
        scheme, host, port_part, path
    );
    let mut tail_url = url::Url::parse(&base)
        .map_err(|e| eyre!("Failed to construct tail URL: {}", e))?;
    tail_url
        .query_pairs_mut()
        .append_pair("query", query)
        .append_pair("start", &start_ns.to_string());

    Ok(tail_url.to_string())
}

/// Stream logs via WebSocket tail. Returns when the connection closes or an
/// error occurs. Callers should check for restart signals externally.
async fn tail_stream(
    base_url: &url::Url,
    query: &str,
    start_ns: i128,
    tx: &mpsc::Sender<SourceMessage>,
    source_id: u16,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<()> {
    let url = make_tail_url(base_url, query, start_ns)?;

    let ws_stream = match tls_config {
        Some(cfg) => {
            let connector = tokio_tungstenite::Connector::Rustls(cfg);
            tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector))
                .await
                .map_err(|e| eyre!("WebSocket connection to '{}' failed: {}", url, e))?
                .0
        }
        None => {
            tokio_tungstenite::connect_async(&url)
                .await
                .map_err(|e| eyre!("WebSocket connection to '{}' failed: {}", url, e))?
                .0
        }
    };

    tracing::info!("WebSocket tail connected to {}", url);

    let (_write, mut read) = ws_stream.split();

    while let Some(msg_result) = read.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("WebSocket read error: {}", e);
                break;
            }
        };

        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => break,
            tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => continue,
            _ => continue,
        };

        let tail_resp: LokiTailResponse = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to parse tail frame: {}", e);
                continue;
            }
        };

        for stream in tail_resp.streams {
            let logs = stream_to_raw_logs(stream, source_id);
            for (_ns, raw_log) in logs {
                if tx.send(SourceMessage::Log(raw_log)).is_err() {
                    return Ok(());
                }
            }
        }
    }

    Ok(())
}

// --- Main entry point ---

/// Run the Loki source. Performs an initial historical backfill via query_range,
/// then optionally streams via WebSocket tail. Listens for restart signals
/// from the TUI via the watch channel.
///
/// `client` is a pre-built reqwest client (pass `reqwest::Client::new()` for
/// plain HTTP/HTTPS; pass a client with mTLS for Teleport sources).
/// `tls_config` is used for the WebSocket tail connection; pass `None` for
/// non-Teleport sources.
pub async fn run_loki_source(
    base_url: url::Url,
    initial_params: LokiSourceParams,
    tx: mpsc::Sender<SourceMessage>,
    mut restart_rx: tokio::sync::watch::Receiver<Option<LokiSourceParams>>,
    source_id: u16,
    client: reqwest::Client,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) {
    let mut params = initial_params;

    loop {
        let end_ns = params
            .end_ns
            .unwrap_or_else(|| jiff::Zoned::now().timestamp().as_nanosecond());

        // Phase 1: Historical backfill.
        tracing::info!(
            "Loki backfill: query={:?} start={} end={} follow={}",
            params.query, params.start_ns, end_ns, params.follow
        );

        let last_ts = tokio::select! {
            result = fetch_all(&client, &base_url, &params.query, params.start_ns, end_ns, &tx, source_id) => {
                match result {
                    Ok(ts) => ts,
                    Err(e) => {
                        tracing::error!("Loki backfill failed: {}", e);
                        // Wait for a restart signal before retrying.
                        if restart_rx.changed().await.is_err() {
                            return;
                        }
                        if let Some(new_params) = restart_rx.borrow_and_update().clone() {
                            let _ = tx.send(SourceMessage::Reset { source_id });
                            params = new_params;
                        }
                        continue;
                    }
                }
            }
            result = restart_rx.changed() => {
                if result.is_err() {
                    return;
                }
                if let Some(new_params) = restart_rx.borrow_and_update().clone() {
                    let _ = tx.send(SourceMessage::Reset { source_id });
                    params = new_params;
                }
                continue;
            }
        };

        // Phase 2: Follow mode via WebSocket tail.
        if !params.follow {
            // No follow — wait for restart or shutdown.
            if restart_rx.changed().await.is_err() {
                return;
            }
            if let Some(new_params) = restart_rx.borrow_and_update().clone() {
                let _ = tx.send(SourceMessage::Reset { source_id });
                params = new_params;
            }
            continue;
        }

        // Inner reconnect loop: only the WebSocket is restarted on disconnect;
        // a restart signal breaks out to the outer loop for a full re-backfill.
        let tail_start = last_ts + 1;
        loop {
            tracing::info!("Loki tail: query={:?} start={}", params.query, tail_start);

            tokio::select! {
                result = tail_stream(&base_url, &params.query, tail_start, &tx, source_id, tls_config.clone()) => {
                    match result {
                        Ok(()) => tracing::info!("WebSocket tail closed, reconnecting..."),
                        Err(e) => tracing::warn!("WebSocket tail error: {}, reconnecting...", e),
                    }
                    // Brief pause to avoid tight reconnect loops, then retry the tail only.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                result = restart_rx.changed() => {
                    if result.is_err() {
                        return;
                    }
                    if let Some(new_params) = restart_rx.borrow_and_update().clone() {
                        let _ = tx.send(SourceMessage::Reset { source_id });
                        params = new_params;
                    }
                    break; // full restart: re-backfill with new params
                }
            }
        }
    }
}
