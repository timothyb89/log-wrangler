use std::collections::HashMap;

use color_eyre::eyre::eyre;
use color_eyre::Result;

pub mod loki;
pub mod stdin;
pub mod teleport;

pub struct RawLog {
    pub timestamp: jiff::Zoned,
    pub message: String,
    pub labels: HashMap<String, String>,
    pub source_id: u16,
}

/// Messages sent from sources to the ingest thread.
pub enum SourceMessage {
    /// A new log entry to ingest.
    Log(RawLog),
    /// Clear entries for a specific source and re-initialize. Sent by the Loki
    /// source before starting a new query.
    Reset { source_id: u16 },
}

/// Parsed source configuration from the `--source` URI.
pub enum SourceConfig {
    Stdin {
        /// Optional format hint from the `?format=` URI query parameter,
        /// e.g. `stdin://?format=slog`.
        format_hint: Option<String>,
    },
    GrafanaLoki { base_url: url::Url },
    /// Grafana+Loki accessed through a Teleport app proxy.
    /// `app_name` is the Teleport app name; `loki_path` is the path on the
    /// proxied host that serves as the Loki base URL.
    GrafanaLokiTeleport { app_name: String, loki_path: String },
}

/// A source with an assigned name and numeric ID.
pub struct NamedSource {
    pub id: u16,
    pub name: String,
    pub config: SourceConfig,
}

/// A parsed `--query` value: optional source name + LogQL string.
pub struct NamedQuery {
    pub name: Option<String>,
    pub query: String,
}

/// Parse a `[name=]<logql>` string into a `NamedQuery`.
///
/// The `name=` prefix is only recognised when the part before `=` looks like a
/// plain identifier (no spaces, braces, pipes, or quotes). This avoids
/// misinterpreting LogQL like `{app="myapp"}` as a named query.
pub fn parse_named_query(raw: &str) -> NamedQuery {
    if let Some((n, rest)) = raw.split_once('=') {
        let looks_like_name = !n.is_empty()
            && !n.contains(|c: char| c.is_whitespace() || "{}\"|`".contains(c));
        if looks_like_name {
            return NamedQuery {
                name: Some(n.to_string()),
                query: rest.to_string(),
            };
        }
    }
    NamedQuery {
        name: None,
        query: raw.to_string(),
    }
}

/// Parse a `[name=]uri` string into a `NamedSource`.
///
/// If no `name=` prefix is provided, a name is auto-generated from the scheme
/// and index (e.g. `stdin`, `loki-0`).
pub fn parse_named_source(raw: &str, index: usize) -> Result<NamedSource> {
    let (name, uri) = if let Some((n, u)) = raw.split_once('=') {
        // Only treat it as name=uri if the part before '=' looks like a plain
        // name (no slashes, colons) rather than part of a URI scheme.
        if !n.contains('/') && !n.contains(':') && !n.is_empty() {
            (Some(n.to_string()), u)
        } else {
            (None, raw)
        }
    } else {
        (None, raw)
    };

    let config = parse_source_uri(uri)?;

    let name = name.unwrap_or_else(|| match &config {
        SourceConfig::Stdin { .. } => "stdin".to_string(),
        SourceConfig::GrafanaLoki { .. } => format!("loki-{}", index),
        SourceConfig::GrafanaLokiTeleport { app_name, .. } => app_name.clone(),
    });

    Ok(NamedSource {
        id: index as u16,
        name,
        config,
    })
}

/// Parse a source URI string into a `SourceConfig`.
///
/// Supported schemes:
/// - `stdin://` (or `stdin`) — read JSONL from stdin
/// - `grafana+loki+http://host:port/path` — Grafana Loki datasource proxy
/// - `grafana+loki+https://host:port/path` — same, over HTTPS
pub fn parse_source_uri(uri: &str) -> Result<SourceConfig> {
    if uri == "stdin" {
        return Ok(SourceConfig::Stdin { format_hint: None });
    }

    if uri.starts_with("stdin://") {
        let url = url::Url::parse(uri)
            .map_err(|e| eyre!("Invalid stdin URI '{}': {}", uri, e))?;
        let format_hint = url
            .query_pairs()
            .find(|(k, _)| k == "format")
            .map(|(_, v)| v.into_owned());
        return Ok(SourceConfig::Stdin { format_hint });
    }

    if let Some(rest) = uri.strip_prefix("grafana+loki+teleport://") {
        // rest = "app-name/api/datasources/proxy/uid/..."
        let (app_name, loki_path) = match rest.find('/') {
            Some(pos) => (&rest[..pos], &rest[pos..]),
            None => (rest, "/"),
        };
        if app_name.is_empty() {
            return Err(eyre!(
                "Missing app name in Teleport source URI '{}'. \
                 Expected 'grafana+loki+teleport://app-name/path'",
                uri
            ));
        }
        return Ok(SourceConfig::GrafanaLokiTeleport {
            app_name: app_name.to_string(),
            loki_path: loki_path.to_string(),
        });
    }

    if let Some(rest) = uri.strip_prefix("grafana+loki+") {
        let url = url::Url::parse(rest)
            .map_err(|e| eyre!("Invalid Grafana Loki URL '{}': {}", rest, e))?;
        return Ok(SourceConfig::GrafanaLoki { base_url: url });
    }

    Err(eyre!(
        "Unknown source URI scheme: '{}'. \
         Expected 'stdin://', 'grafana+loki+http://...', or 'grafana+loki+teleport://app-name/path'",
        uri
    ))
}
