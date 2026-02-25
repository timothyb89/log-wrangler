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

    Ok(())
}
