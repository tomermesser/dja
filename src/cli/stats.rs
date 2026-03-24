use crate::cache::CacheDb;
use crate::config::Config;
use anyhow::Result;

/// Run the `dja stats` command.
pub async fn run(json: bool) -> Result<()> {
    let db_path = Config::data_dir().join("cache.db");

    if !db_path.exists() {
        anyhow::bail!("Cache database not found. Run `dja init` first.");
    }

    let db = CacheDb::open(&db_path).await?;
    let entry_count = db.entry_count().await?;
    let total_size = db.total_size().await?;
    let total_hits = db.total_hits().await?;

    let db_file_size = std::fs::metadata(&db_path)
        .map(|m| m.len())
        .unwrap_or(0);

    if json {
        let stats = serde_json::json!({
            "entry_count": entry_count,
            "total_response_size": total_size,
            "total_hits": total_hits,
            "db_file_size": db_file_size,
        });
        println!("{}", serde_json::to_string_pretty(&stats)?);
    } else {
        println!("Cache Statistics");
        println!("  Entries:       {}", entry_count);
        println!("  Response data: {}", human_readable_size(total_size));
        println!("  Total hits:    {}", total_hits);
        println!("  DB file size:  {}", human_readable_size(db_file_size));
    }

    Ok(())
}

fn human_readable_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
