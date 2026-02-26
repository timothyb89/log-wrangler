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
use source::{SourceConfig, SourceMessage};

#[tokio::main]
async fn main() -> Result<()> {
    util::install_tracing();
    color_eyre::install()?;

    let args = Args::parse();
    let source_config = source::parse_source_uri(&args.source)?;

    let arena = log::Arena::new();

    // Create channel for log ingestion. Sources send SourceMessages into `tx`.
    let (tx, rx) = mpsc::channel::<SourceMessage>();

    // Spawn the ingest thread (blocking, uses std mpsc).
    let arena_clone = arena.clone();
    std::thread::spawn(move || {
        log::ingest(rx, arena_clone);
    });

    // Source-specific setup.
    let restart_tx: Option<tokio::sync::watch::Sender<Option<LokiSourceParams>>>;
    let initial_query: Option<String>;

    match source_config {
        SourceConfig::Stdin => {
            restart_tx = None;
            initial_query = None;

            // Spawn stdin reader only when stdin is piped (not a TTY).
            // When stdin is a TTY, crossterm needs exclusive access for keyboard events.
            if !std::io::stdin().is_terminal() {
                let tx_stdin = tx.clone();
                std::thread::spawn(move || {
                    source::stdin::read_stdin(tx_stdin);
                });
            }
        }
        SourceConfig::GrafanaLoki { base_url } => {
            let query = args
                .query
                .ok_or_else(|| eyre!("--query is required for Loki sources"))?;

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
            restart_tx = Some(wtx);
            initial_query = Some(query);

            let tx_loki = tx.clone();
            tokio::spawn(source::loki::run_loki_source(
                base_url, params, tx_loki, wrx,
            ));
        }
    }

    // Drop our copy so ingest thread can detect when all sources close.
    drop(tx);

    // Run the TUI on the async runtime.
    sink::tui::run_tui(arena, restart_tx, initial_query).await?;

    Ok(())
}
