use crate::config::Config;
use crate::embedding::download::default_model_dir;
use anyhow::Result;

/// Run the `dja verify` command: check health of model, DB, daemon, and config.
pub fn run() -> Result<()> {
    let mut all_ok = true;

    // 1. Check config
    print!("Config:    ");
    let config_path = Config::config_path();
    if config_path.exists() {
        match Config::load() {
            Ok(_) => println!("OK ({})", config_path.display()),
            Err(e) => {
                println!("ERROR - {}", e);
                all_ok = false;
            }
        }
    } else {
        println!("MISSING ({})", config_path.display());
        all_ok = false;
    }

    // 2. Check model files
    print!("Model:     ");
    match default_model_dir() {
        Ok(model_dir) => {
            let model_ok = model_dir.join("model.onnx").exists();
            let tokenizer_ok = model_dir.join("tokenizer.json").exists();
            if model_ok && tokenizer_ok {
                println!("OK ({})", model_dir.display());
            } else {
                let mut missing = Vec::new();
                if !model_ok {
                    missing.push("model.onnx");
                }
                if !tokenizer_ok {
                    missing.push("tokenizer.json");
                }
                println!("MISSING files: {}", missing.join(", "));
                all_ok = false;
            }
        }
        Err(e) => {
            println!("ERROR - {}", e);
            all_ok = false;
        }
    }

    // 3. Check database
    print!("Database:  ");
    let db_path = Config::data_dir().join("cache.db");
    if db_path.exists() {
        let size = std::fs::metadata(&db_path)
            .map(|m| m.len())
            .unwrap_or(0);
        println!("OK ({}, {} bytes)", db_path.display(), size);
    } else {
        println!("MISSING ({})", db_path.display());
        all_ok = false;
    }

    // 4. Check daemon
    print!("Daemon:    ");
    let pid_path = Config::pid_path();
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                let alive = unsafe { libc::kill(pid, 0) == 0 };
                if alive {
                    println!("RUNNING (PID {})", pid);
                } else {
                    println!("NOT RUNNING (stale PID file)");
                }
            } else {
                println!("NOT RUNNING (invalid PID file)");
            }
        } else {
            println!("NOT RUNNING (cannot read PID file)");
        }
    } else {
        println!("NOT RUNNING");
    }

    // Summary
    println!();
    if all_ok {
        println!("All checks passed.");
    } else {
        println!("Some checks failed. Run `dja init` to fix setup issues.");
    }

    Ok(())
}
