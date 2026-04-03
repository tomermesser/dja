use crate::config::Config;
use anyhow::Result;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// Run the `dja uninstall` command.
pub fn run(force: bool) -> Result<()> {
    // Stop daemon if running
    let pid_path = Config::pid_path();
    if pid_path.exists() {
        if let Ok(pid_str) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                let alive = unsafe { libc::kill(pid, 0) == 0 };
                if alive {
                    println!("Stopping dja daemon (PID {pid})...");
                    unsafe { libc::kill(pid, libc::SIGTERM) };
                    // Wait briefly for clean shutdown
                    for _ in 0..20 {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if unsafe { libc::kill(pid, 0) != 0 } {
                            break;
                        }
                    }
                }
            }
        }
    }

    let data_dir = Config::data_dir();
    let config_dir = Config::config_path()
        .parent()
        .expect("config path has parent")
        .to_path_buf();
    let binary_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("dja"));
    let shell_profile = detect_shell_profile();

    println!();
    println!("This will remove:");
    println!("  Binary:      {}", binary_path.display());
    println!("  Data:        {}", data_dir.display());
    println!("  Config:      {}", config_dir.display());
    if let Some(ref profile) = shell_profile {
        println!("  Shell hooks: {} (dja block only)", profile.display());
    }
    println!();

    if !force {
        print!("Continue? [y/N]: ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Uninstall cancelled.");
            return Ok(());
        }
    }

    // Remove shell integration
    if let Some(ref profile) = shell_profile {
        if remove_shell_integration(profile)? {
            println!("Removed shell integration from {}", profile.display());
        }
    }

    // Remove data directory
    if data_dir.exists() {
        fs::remove_dir_all(&data_dir)?;
        println!("Removed {}", data_dir.display());
    }

    // Remove config directory
    if config_dir.exists() {
        fs::remove_dir_all(&config_dir)?;
        println!("Removed {}", config_dir.display());
    }

    // Remove binary
    if binary_path.exists() {
        fs::remove_file(&binary_path)?;
        println!("Removed {}", binary_path.display());
    }

    println!();
    println!("dja has been uninstalled.");
    if shell_profile.is_some() {
        println!("Restart your shell to clear environment variables.");
    }

    Ok(())
}

/// Find the user's shell profile file.
fn detect_shell_profile() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    if shell.ends_with("/zsh") {
        Some(home.join(".zshrc"))
    } else if shell.ends_with("/bash") {
        Some(home.join(".bashrc"))
    } else {
        None
    }
}

/// Remove the dja shell integration block from the profile file.
/// Returns true if something was removed.
fn remove_shell_integration(profile: &PathBuf) -> Result<bool> {
    let contents = match fs::read_to_string(profile) {
        Ok(c) => c,
        Err(_) => return Ok(false),
    };

    let marker = "# dja shell integration";
    if !contents.contains(marker) {
        return Ok(false);
    }

    // The block starts with a blank line + marker and ends before the next
    // non-dja content. We remove from the marker line to the end of the
    // `_dja_sync_env` call (the last line of the block).
    let mut lines: Vec<&str> = contents.lines().collect();
    let start = lines.iter().position(|l| l.contains(marker));
    if let Some(start_idx) = start {
        // Find the end: the _dja_sync_env standalone call at the end of the block
        let mut end_idx = start_idx;
        for (i, line) in lines[start_idx..].iter().enumerate() {
            if *line == "_dja_sync_env" {
                end_idx = start_idx + i;
                break;
            }
        }
        // Also remove the blank line before the marker if present
        let remove_start = if start_idx > 0 && lines[start_idx - 1].is_empty() {
            start_idx - 1
        } else {
            start_idx
        };
        lines.drain(remove_start..=end_idx);

        let mut new_contents = lines.join("\n");
        // Ensure file ends with a newline
        if !new_contents.ends_with('\n') {
            new_contents.push('\n');
        }
        fs::write(profile, new_contents)?;
        return Ok(true);
    }

    Ok(false)
}
