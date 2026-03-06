use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::eyre::eyre;
use color_eyre::Result;

mod cli;
mod filter;
mod log;
mod sink;
mod source;
mod util;

use cli::Args;
use source::loki::LokiSourceParams;
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

    // Spawn the ingest thread (blocking, uses std mpsc).
    let arena_clone = arena.clone();
    let reorder_buffer = args.reorder_buffer;
    std::thread::spawn(move || {
        log::ingest(rx, arena_clone, reorder_buffer);
    });

    // Spawn each source and collect per-source restart senders for Loki sources.
    let mut loki_restarts: Vec<sink::tui::SourceRestart> = Vec::new();
    let num_cli_sources = sources.len();

    for src in sources {
        match src.config {
            SourceConfig::Stdin => {
                // Spawn stdin reader only when stdin is piped (not a TTY).
                // When stdin is a TTY, crossterm needs exclusive access for keyboard events.
                if !std::io::stdin().is_terminal() {
                    let tx_stdin = tx.clone();
                    let sid = src.id;
                    std::thread::spawn(move || {
                        source::stdin::read_stdin(tx_stdin, sid);
                    });
                }
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

                let start_ns = start.timestamp().as_nanosecond();
                let end_ns = end.timestamp().as_nanosecond();

                let params = LokiSourceParams {
                    query: query.clone(),
                    start_ns,
                    end_ns: Some(end_ns),
                    follow: args.follow,
                };

                let (wtx, wrx) = tokio::sync::watch::channel(None);

                loki_restarts.push(sink::tui::SourceRestart {
                    source_id: src.id,
                    name: src.name,
                    tx: wtx,
                    query,
                });

                let tx_loki = tx.clone();
                let sid = src.id;
                tokio::spawn(source::loki::run_loki_source(
                    base_url, params, tx_loki, wrx, sid,
                ));
            }
        }
    }

    let next_source_id = num_cli_sources as u16;

    // Run the TUI on the async runtime.
    sink::tui::run_tui(arena, loki_restarts, tx, next_source_id).await?;

    Ok(())
}
