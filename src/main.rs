use std::io::IsTerminal;
use std::sync::mpsc;

use clap::Parser;
use color_eyre::Result;

mod cli;
mod filter;
mod log;
mod sink;
mod source;
mod util;

use cli::Args;

#[tokio::main]
async fn main() -> Result<()> {
    util::install_tracing();
    color_eyre::install()?;

    let args = Args::parse();

    let arena = log::Arena::new();

    // Create channel for log ingestion. Sources will send RawLogs into `tx`.
    let (tx, rx) = mpsc::channel::<source::RawLog>();

    // Spawn the ingest thread (blocking, uses std mpsc).
    let arena_clone = arena.clone();
    std::thread::spawn(move || {
        log::ingest(rx, arena_clone);
    });

    // Spawn stdin reader only when stdin is piped (not a TTY).
    // When stdin is a TTY, crossterm needs exclusive access to it for keyboard events.
    if !std::io::stdin().is_terminal() {
        let tx_stdin = tx.clone();
        std::thread::spawn(move || {
            source::stdin::read_stdin(tx_stdin);
        });
    }

    // Drop our copy so ingest thread can detect when all sources close.
    drop(tx);

    // Run the TUI on the async runtime.
    sink::tui::run_tui(arena).await?;

    Ok(())
}
