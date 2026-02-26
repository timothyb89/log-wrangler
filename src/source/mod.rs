use std::collections::HashMap;

use color_eyre::eyre::eyre;
use color_eyre::Result;

pub mod loki;
pub mod stdin;

pub struct RawLog {
    pub timestamp: jiff::Zoned,
    pub message: String,
    pub labels: HashMap<String, String>,
}

/// Messages sent from sources to the ingest thread.
pub enum SourceMessage {
    /// A new log entry to ingest.
    Log(RawLog),
    /// Clear all arena data and re-initialize. Sent by the Loki source
    /// before starting a new query so the ingest thread can re-clone rodeos.
    Reset,
}

/// Parsed source configuration from the `--source` URI.
pub enum SourceConfig {
    Stdin,
    GrafanaLoki { base_url: url::Url },
}

/// Parse a source URI string into a `SourceConfig`.
///
/// Supported schemes:
/// - `stdin://` (or `stdin`) — read JSONL from stdin
/// - `grafana+loki+http://host:port/path` — Grafana Loki datasource proxy
/// - `grafana+loki+https://host:port/path` — same, over HTTPS
pub fn parse_source_uri(uri: &str) -> Result<SourceConfig> {
    if uri == "stdin://" || uri == "stdin" {
        return Ok(SourceConfig::Stdin);
    }

    if let Some(rest) = uri.strip_prefix("grafana+loki+") {
        let url = url::Url::parse(rest)
            .map_err(|e| eyre!("Invalid Grafana Loki URL '{}': {}", rest, e))?;
        return Ok(SourceConfig::GrafanaLoki { base_url: url });
    }

    Err(eyre!(
        "Unknown source URI scheme: '{}'. Expected 'stdin://' or 'grafana+loki+http://...'",
        uri
    ))
}
