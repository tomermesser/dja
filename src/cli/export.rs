use crate::cache::CacheDb;
use crate::config::Config;
use anyhow::Result;
use base64::Engine;
use serde::Serialize;

#[derive(Serialize)]
struct ExportEntry {
    prompt_text: String,
    model: String,
    system_hash: String,
    response_data: String, // base64 encoded
}

/// Run the `dja export` command.
pub async fn run() -> Result<()> {
    let db_path = Config::data_dir().join("cache.db");

    if !db_path.exists() {
        anyhow::bail!("Cache database not found. Run `dja init` first.");
    }

    let db = CacheDb::open(&db_path).await?;
    let entries = db.export_all().await?;

    let export_entries: Vec<ExportEntry> = entries
        .into_iter()
        .map(|e| ExportEntry {
            prompt_text: e.prompt_text,
            model: e.model,
            system_hash: e.system_hash,
            response_data: base64::engine::general_purpose::STANDARD.encode(&e.response_data),
        })
        .collect();

    let json = serde_json::to_string_pretty(&export_entries)?;
    println!("{json}");

    eprintln!("Exported {} entries.", export_entries.len());
    Ok(())
}
