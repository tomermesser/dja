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
    /// Initialize dja: config, model, database
    Init {
        /// Add ANTHROPIC_BASE_URL export to shell profile
        #[arg(long)]
        global: bool,
    },
    /// Show cache statistics
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show ASCII bar chart of cache hits by day (last 7 days)
        #[arg(long)]
        graph: bool,
    },
    /// Clear cache entries
    Clear {
        /// Delete entries older than duration (e.g. "30d", "24h")
        #[arg(long)]
        older_than: Option<String>,
    },
    /// View or modify configuration
    Config {
        /// Config key to get or set
        key: Option<String>,
        /// Value to set (requires key)
        value: Option<String>,
    },
    /// Test embedding and cache lookup for a prompt
    Test {
        /// The prompt text to test
        prompt: String,
    },
    /// Show recent log output
    Log,
    /// Verify installation health
    Verify,
    /// Export cache as JSON
    Export,
    /// Import cache from JSON file
    Import {
        /// Path to JSON file to import
        file: String,
    },
    /// Open live TUI dashboard
    Monitor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start => dja::cli::start::run().await?,
        Commands::Stop => dja::cli::stop()?,
        Commands::Status => dja::cli::status()?,
        Commands::Init { global } => dja::cli::init::run(global).await?,
        Commands::Stats { json, graph } => dja::cli::stats::run(json, graph).await?,
        Commands::Clear { older_than } => dja::cli::clear::run(older_than).await?,
        Commands::Config { key, value } => dja::cli::config_cmd::run(key, value)?,
        Commands::Test { prompt } => dja::cli::test_cmd::run(prompt).await?,
        Commands::Log => dja::cli::log::run()?,
        Commands::Verify => dja::cli::verify::run()?,
        Commands::Export => dja::cli::export::run().await?,
        Commands::Import { file } => dja::cli::import::run(file).await?,
        Commands::Monitor => dja::cli::monitor::run().await?,
    }

    Ok(())
}
