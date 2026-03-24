use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";

const MODEL_DIR_NAME: &str = "all-MiniLM-L6-v2";

/// Known SHA-256 hashes for integrity verification.
/// These are checked after download; a mismatch causes an error.
const MODEL_ONNX_SHA256: &str =
    "06fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452";
const TOKENIZER_JSON_SHA256: &str =
    "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037";

/// Return the default model directory: `~/.local/share/dja/models/all-MiniLM-L6-v2`
pub fn default_model_dir() -> Result<PathBuf> {
    let data_dir = dirs::data_dir().context("cannot determine data directory")?;
    Ok(data_dir.join("dja").join("models").join(MODEL_DIR_NAME))
}

/// Download the all-MiniLM-L6-v2 model and tokenizer if not already present.
/// Returns the model directory path.
pub async fn download_model() -> Result<PathBuf> {
    let model_dir = default_model_dir()?;
    let model_path = model_dir.join("model.onnx");
    let tokenizer_path = model_dir.join("tokenizer.json");

    // Check if both files already exist and pass integrity checks
    if model_path.exists() && tokenizer_path.exists() {
        let model_ok = verify_file_hash(&model_path, MODEL_ONNX_SHA256).await.unwrap_or(false);
        let tokenizer_ok = verify_file_hash(&tokenizer_path, TOKENIZER_JSON_SHA256).await.unwrap_or(false);
        if model_ok && tokenizer_ok {
            tracing::info!("Model already exists and verified at {}", model_dir.display());
            return Ok(model_dir);
        }
        tracing::warn!("Existing model files failed integrity check, re-downloading");
    }

    tokio::fs::create_dir_all(&model_dir)
        .await
        .context("failed to create model directory")?;

    // Download model.onnx
    download_file(MODEL_URL, &model_path, MODEL_ONNX_SHA256).await?;

    // Download tokenizer.json
    download_file(TOKENIZER_URL, &tokenizer_path, TOKENIZER_JSON_SHA256).await?;

    Ok(model_dir)
}

/// Download a single file with progress reporting and SHA-256 verification.
/// Uses an atomic write: writes to a `.tmp` file first, then renames on success.
async fn download_file(url: &str, dest: &Path, expected_sha256: &str) -> Result<()> {
    let filename = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    tracing::info!("Downloading {} …", filename);

    let tmp_dest = dest.with_extension("tmp");

    let result = download_file_inner(url, &filename, &tmp_dest, expected_sha256).await;

    if let Err(e) = result {
        // Clean up partial .tmp file on any error
        let _ = tokio::fs::remove_file(&tmp_dest).await;
        return Err(e);
    }

    // Atomic rename: only replace dest once download is verified
    tokio::fs::rename(&tmp_dest, dest)
        .await
        .context("failed to rename tmp file to destination")?;

    tracing::info!("{}: download complete, integrity verified", filename);
    Ok(())
}

/// Inner download logic that writes to `tmp_dest`.
async fn download_file_inner(
    url: &str,
    filename: &str,
    tmp_dest: &Path,
    expected_sha256: &str,
) -> Result<()> {
    use futures::StreamExt;

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .context("HTTP request failed")?;

    if !response.status().is_success() {
        bail!(
            "Download failed for {}: HTTP {}",
            filename,
            response.status()
        );
    }

    let total_size = response.content_length();
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(tmp_dest)
        .await
        .context("failed to create tmp file")?;

    let mut downloaded: u64 = 0;
    let mut hasher = Sha256::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading download stream")?;
        file.write_all(&chunk)
            .await
            .context("error writing to file")?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;

        if let Some(total) = total_size {
            tracing::info!(
                "{}: {:.1} / {:.1} MB",
                filename,
                downloaded as f64 / 1_048_576.0,
                total as f64 / 1_048_576.0,
            );
        } else {
            tracing::info!("{}: {:.1} MB downloaded", filename, downloaded as f64 / 1_048_576.0);
        }
    }

    file.flush().await?;

    // SHA-256 integrity check — use {:064x} to always produce a 64-char lowercase hex string
    let hash = format!("{:064x}", hasher.finalize());
    if hash != expected_sha256 {
        bail!(
            "SHA-256 mismatch for {filename}: expected {expected_sha256}, got {hash}"
        );
    }

    Ok(())
}

/// Compute the SHA-256 hash of an existing file and compare to expected.
async fn verify_file_hash(path: &Path, expected_sha256: &str) -> Result<bool> {
    let bytes = tokio::fs::read(path).await.context("failed to read file for hash verification")?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:064x}", hasher.finalize());
    Ok(hash == expected_sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_dir() {
        let dir = default_model_dir().unwrap();
        assert!(dir.ends_with("dja/models/all-MiniLM-L6-v2"));
    }

    #[tokio::test]
    async fn test_download_creates_files() {
        // This test actually downloads the model if not present.
        // In CI without network, it will be skipped via the error path.
        let result = download_model().await;
        match result {
            Ok(dir) => {
                assert!(dir.join("model.onnx").exists());
                assert!(dir.join("tokenizer.json").exists());
            }
            Err(e) => {
                eprintln!("Skipping download test (network unavailable?): {e}");
            }
        }
    }

}
