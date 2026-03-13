use std::collections::HashMap;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::eyre::eyre;
use color_eyre::Result;

mod cli;
mod filter;
mod format;
mod log;
mod sink;
mod source;
mod util;

use cli::Args;
use source::loki::LokiSourceParams;
use source::teleport::TeleportTlsConfig;
use source::{NamedQuery, NamedSource, SourceConfig, SourceMessage};

#[tokio::main]
async fn main() -> Result<()> {
    util::install_tracing();
    color_eyre::install()?;

    let args = Args::parse();

    // Parse all source URIs.
    let sources: Vec<NamedSource> = args
        .source
        .iter()
        .enumerate()
        .map(|(i, raw)| source::parse_named_source(raw, i))
        .collect::<Result<_>>()?;

    // Parse queries and build a name→query map.
    let queries: Vec<NamedQuery> = args
        .query
        .iter()
        .map(|raw| source::parse_named_query(raw))
        .collect();

    // Separate into named and default (unnamed) queries.
    let mut named_queries: HashMap<String, String> = HashMap::new();
    let mut default_query: Option<String> = None;
    for q in queries {
        match q.name {
            Some(name) => {
                if named_queries.contains_key(&name) {
                    return Err(eyre!("Duplicate --query for source '{}'", name));
                }
                named_queries.insert(name, q.query);
            }
            None => {
                if default_query.is_some() {
                    return Err(eyre!(
                        "Multiple unnamed --query flags; use name= prefix to target specific sources"
                    ));
                }
                default_query = Some(q.query);
            }
        }
    }

    let arena = log::Arena::new();

    // Register source names in the arena.
    {
        let mut a = arena.lock().unwrap();
        for src in &sources {
            // Ensure vec is large enough (source IDs may not be contiguous
            // with the internal source at u16::MAX, but user sources are 0..N).
            if src.id as usize >= a.source_names.len() {
                a.source_names.resize(src.id as usize + 1, String::new());
            }
            a.source_names[src.id as usize] = src.name.clone();
        }
    }

    // Create channel for log ingestion. Sources send SourceMessages into `tx`.
    let (tx, rx) = mpsc::channel::<SourceMessage>();

    // Route internal tracing events into the arena as native log entries.
    util::set_internal_log_sender(tx.clone());

    // Build a classifier chain for each source (indexed by source_id).
    let classifiers: Vec<format::ClassifierChain> = sources
        .iter()
        .map(|src| build_classifier_chain(&src.config, args.format_regex.as_deref()))
        .collect();

    // Spawn the ingest thread (blocking, uses std mpsc).
    let arena_clone = arena.clone();
    let reorder_buffer = args.reorder_buffer;
    let default_chain = format::ClassifierChain::new(vec![Box::new(format::json::default())]);
    std::thread::spawn(move || {
        log::ingest(rx, arena_clone, reorder_buffer, classifiers, default_chain);
    });

    // Resolve any Teleport sources before opening the TUI so that `tsh` can
    // freely interact with the terminal (prompt for MFA, etc.).
    let mut teleport_resolved: HashMap<u16, (url::Url, TeleportTlsConfig)> = HashMap::new();
    for src in &sources {
        if let SourceConfig::GrafanaLokiTeleport { app_name, loki_path } = &src.config {
            let tsh_cfg = source::teleport::fetch_tsh_app_config(app_name)?;
            let base_url = tsh_cfg.uri.join(loki_path)
                .map_err(|e| eyre!("Failed to construct Teleport base URL: {}", e))?;
            let tls = source::teleport::build_tls_config(&tsh_cfg)?;
            teleport_resolved.insert(src.id, (base_url, tls));
        }
    }

    // Spawn each source and collect managed-source state for stoppable sources.
    let mut managed_sources: Vec<sink::tui::ManagedSource> = Vec::new();
    let num_cli_sources = sources.len();

    for src in sources {
        match src.config {
            SourceConfig::Stdin { .. } => {
                managed_sources.push(sink::tui::ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: sink::tui::ManagedSourceKind::Stdin,
                });
                let tx_stdin = tx.clone();
                let sid = src.id;
                std::thread::spawn(move || {
                    let stdin = std::io::stdin();
                    source::stdin::read_stdin(tx_stdin, sid, stdin.lock());
                });
            }
            SourceConfig::GrafanaLoki { base_url } => {
                let query = named_queries
                    .remove(&src.name)
                    .or_else(|| default_query.clone())
                    .ok_or_else(|| {
                        eyre!(
                            "--query is required for Loki source '{}'. \
                             Use --query '{}=<logql>' or a bare --query '<logql>'",
                            src.name,
                            src.name,
                        )
                    })?;

                let now = jiff::Zoned::now();
                let start = cli::resolve_start_time(&args.start, &args.since, &now)?;
                let end = cli::resolve_end_time(&args.end, &now)?;

                let params = LokiSourceParams {
                    query: query.clone(),
                    start_ns: start.timestamp().as_nanosecond(),
                    end_ns: Some(end.timestamp().as_nanosecond()),
                    follow: args.follow,
                };

                let (wtx, wrx) = tokio::sync::watch::channel(None);

                managed_sources.push(sink::tui::ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: sink::tui::ManagedSourceKind::Loki {
                        base_url: base_url.clone(),
                        query,
                        tx: wtx,
                        tls: None,
                    },
                });

                let tx_loki = tx.clone();
                let sid = src.id;
                tokio::spawn(source::loki::run_loki_source(
                    base_url, params, tx_loki, wrx, sid,
                    reqwest::Client::new(), None,
                ));
            }
            SourceConfig::GrafanaLokiTeleport { .. } => {
                let (base_url, tls) = teleport_resolved.remove(&src.id)
                    .expect("Teleport source was resolved above");

                let query = named_queries
                    .remove(&src.name)
                    .or_else(|| default_query.clone())
                    .ok_or_else(|| {
                        eyre!(
                            "--query is required for Teleport source '{}'. \
                             Use --query '{}=<logql>' or a bare --query '<logql>'",
                            src.name,
                            src.name,
                        )
                    })?;

                let now = jiff::Zoned::now();
                let start = cli::resolve_start_time(&args.start, &args.since, &now)?;
                let end = cli::resolve_end_time(&args.end, &now)?;

                let params = LokiSourceParams {
                    query: query.clone(),
                    start_ns: start.timestamp().as_nanosecond(),
                    end_ns: Some(end.timestamp().as_nanosecond()),
                    follow: args.follow,
                };

                let (wtx, wrx) = tokio::sync::watch::channel(None);

                let http_client = tls.http_client.clone();
                let ws_tls = Some(tls.rustls_config.clone());

                managed_sources.push(sink::tui::ManagedSource {
                    source_id: src.id,
                    name: src.name,
                    kind: sink::tui::ManagedSourceKind::Loki {
                        base_url: base_url.clone(),
                        query,
                        tx: wtx,
                        tls: Some(tls),
                    },
                });

                let tx_loki = tx.clone();
                let sid = src.id;
                tokio::spawn(source::loki::run_loki_source(
                    base_url, params, tx_loki, wrx, sid,
                    http_client, ws_tls,
                ));
            }
        }
    }

    let next_source_id = num_cli_sources as u16;

    // Run the TUI on the async runtime.
    sink::tui::run_tui(arena, managed_sources, tx, next_source_id).await?;

    Ok(())
}

fn build_classifier_chain(
    config: &SourceConfig,
    format_regex: Option<&str>,
) -> format::ClassifierChain {
    let hint = match config {
        SourceConfig::Stdin { format_hint } => format_hint.as_deref(),
        _ => None,
    };

    match hint {
        Some("slog") => {
            format::ClassifierChain::new(vec![Box::new(format::json::slog())])
        }
        Some("rust-tracing") => {
            format::ClassifierChain::new(vec![Box::new(format::json::rust_tracing())])
        }
        Some("journald-json") => {
            format::ClassifierChain::new(vec![Box::new(format::json::journald_json())])
        }
        Some("generic") => {
            format::ClassifierChain::new(vec![Box::new(format::plaintext::GenericClassifier)])
        }
        Some("regex") => {
            if let Some(pattern) = format_regex {
                match regex::Regex::new(pattern) {
                    Ok(re) => format::ClassifierChain::new(vec![
                        Box::new(format::plaintext::RegexClassifier { pattern: re }),
                    ]),
                    Err(e) => {
                        eprintln!("Warning: invalid --format-regex pattern: {e}");
                        format::ClassifierChain::new(vec![Box::new(format::json::default())])
                    }
                }
            } else {
                eprintln!("Warning: format=regex requires --format-regex");
                format::ClassifierChain::new(vec![Box::new(format::json::default())])
            }
        }
        _ => format::ClassifierChain::new(vec![Box::new(format::json::default())]),
    }
}
