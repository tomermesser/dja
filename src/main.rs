mod cache;
mod cli;
mod config;
mod proxy;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dja", version, about = "Semantic cache proxy for AI coding tools")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the proxy daemon
    Start,
    /// Stop the proxy daemon
    Stop,
    /// Show daemon status
    Status,
    // TODO: Future subcommands (cache, config, stats, etc.)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start => cli::start::run().await?,
        Commands::Stop => cli::stop()?,
        Commands::Status => cli::status()?,
    }

    Ok(())
}
