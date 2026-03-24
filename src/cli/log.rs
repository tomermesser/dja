use crate::config::Config;
use anyhow::Result;

/// Run the `dja log` command: read/tail the log file.
pub fn run() -> Result<()> {
    let log_path = Config::log_path();

    if !log_path.exists() {
        println!("No log file found at {}", log_path.display());
        println!("The log file is created when the daemon starts.");
        return Ok(());
    }

    let contents = std::fs::read_to_string(&log_path)?;

    if contents.is_empty() {
        println!("Log file is empty.");
        return Ok(());
    }

    // Show the last 50 lines
    let lines: Vec<&str> = contents.lines().collect();
    let start = if lines.len() > 50 {
        lines.len() - 50
    } else {
        0
    };

    if start > 0 {
        println!("... (showing last 50 of {} lines)\n", lines.len());
    }

    for line in &lines[start..] {
        println!("{}", line);
    }

    Ok(())
}
