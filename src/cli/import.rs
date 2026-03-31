use crate::cache::{CacheDb, EMBEDDING_DIM};
use crate::config::Config;
use crate::embedding::EmbeddingModel;
use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;

#[derive(Deserialize)]
struct ImportEntry {
    prompt_text: String,
    model: String,
    system_hash: String,
    response_data: String, // base64 encoded
}

/// Run the `dja import` command.
pub async fn run(file: String) -> Result<()> {
    let db_path = Config::data_dir().join("cache.db");

    if !db_path.exists() {
        anyhow::bail!("Cache database not found. Run `dja init` first.");
    }

    let db = CacheDb::open(&db_path).await?;

    // Load embedding model to re-compute embeddings
    let model_dir = Config::data_dir().join("models").join("all-MiniLM-L6-v2");
    let mut embedding_model = EmbeddingModel::load(&model_dir)
        .with_context(|| format!("Failed to load embedding model from {}", model_dir.display()))?;

    // Read JSON file
    let json_str = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read import file: {file}"))?;
    let entries: Vec<ImportEntry> =
        serde_json::from_str(&json_str).context("Failed to parse import JSON")?;

    let mut imported = 0u64;
    let mut skipped = 0u64;

    for entry in &entries {
        // Check for duplicates
        if db
            .exists(&entry.prompt_text, &entry.system_hash, &entry.model)
            .await?
        {
            skipped += 1;
            continue;
        }

        // Decode response_data
        let response_data = base64::engine::general_purpose::STANDARD
            .decode(&entry.response_data)
            .with_context(|| "Failed to decode base64 response_data")?;

        // Re-compute embedding
        let embedding = embedding_model
            .embed(&entry.prompt_text)
            .context("Failed to compute embedding")?;

        assert_eq!(embedding.len(), EMBEDDING_DIM);

        db.store(
            &entry.prompt_text,
            &entry.system_hash,
            &entry.model,
            &embedding,
            &response_data,
            "local",
        )
        .await?;

        imported += 1;
    }

    println!("Imported {imported} entries, skipped {skipped} duplicates.");
    Ok(())
}
