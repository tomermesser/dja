use crate::cache::CacheDb;
use crate::config::Config;
use crate::embedding::download::default_model_dir;
use crate::embedding::EmbeddingModel;
use anyhow::Result;

/// Run the `dja test` command: embed a prompt and search the cache.
pub async fn run(prompt: String) -> Result<()> {
    let config = Config::load()?;

    // Load embedding model
    let model_dir = default_model_dir()?;
    if !model_dir.join("model.onnx").exists() {
        anyhow::bail!("Model not found. Run `dja init` first.");
    }

    println!("Loading embedding model...");
    let mut model = EmbeddingModel::load(&model_dir)?;

    // Embed the prompt
    println!("Embedding prompt...");
    let embedding = model.embed(&prompt)?;
    println!("Embedding dimension: {}", embedding.len());

    // Search cache
    let db_path = Config::data_dir().join("cache.db");
    if !db_path.exists() {
        println!("No cache database found. Run `dja init` first.");
        return Ok(());
    }

    let db = CacheDb::open(&db_path).await?;
    let entry_count = db.entry_count().await?;
    println!("Cache entries: {}", entry_count);

    if entry_count == 0 {
        println!("Cache is empty, no lookup to perform.");
        return Ok(());
    }

    // Use a dummy system_hash and model for testing lookups
    let result = db
        .lookup(&embedding, "", "", config.threshold as f32)
        .await?;

    match result {
        Some(hit) => {
            println!("Cache HIT!");
            println!("  Similarity: {:.4}", hit.similarity);
            println!("  Matched prompt: {}", hit.prompt_text);
            println!(
                "  Response size: {} bytes",
                hit.response_data.len()
            );
        }
        None => {
            println!(
                "No cache hit (threshold: {:.2})",
                config.threshold
            );
        }
    }

    Ok(())
}
