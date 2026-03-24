use anyhow::{Context, Result};
use ort::session::Session;
use ort::value::Tensor;
use std::path::Path;

use super::tokenizer::DjaTokenizer;

/// Embedding model backed by ONNX Runtime and a HuggingFace tokenizer.
pub struct EmbeddingModel {
    session: Session,
    tokenizer: DjaTokenizer,
}

impl EmbeddingModel {
    /// Load the ONNX model and tokenizer from `model_dir`.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let model_path = model_dir.join("model.onnx");

        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to load ONNX model")?;

        let tokenizer = DjaTokenizer::load(model_dir)?;

        Ok(Self { session, tokenizer })
    }

    /// Embed a text string into a 384-dimensional vector.
    ///
    /// Pipeline: tokenize -> ONNX inference -> mean pooling (masked) -> L2 normalize.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer.tokenize(text)?;

        let seq_len = tokens.input_ids.len();

        // Build input tensors with shape [1, seq_len]
        let input_ids = Tensor::from_array(([1usize, seq_len], tokens.input_ids.clone().into_boxed_slice()))
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let attention_mask = Tensor::from_array(([1usize, seq_len], tokens.attention_mask.clone().into_boxed_slice()))
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let token_type_ids = Tensor::from_array(([1usize, seq_len], tokens.token_type_ids.clone().into_boxed_slice()))
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let outputs = self
            .session
            .run(ort::inputs!["input_ids" => input_ids, "attention_mask" => attention_mask, "token_type_ids" => token_type_ids])
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("ONNX inference failed")?;

        // The model output "last_hidden_state" has shape [1, seq_len, 384]
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to extract output tensor")?;

        let hidden_size = shape[2] as usize;
        anyhow::ensure!(hidden_size == 384, "expected 384-dim model, got {hidden_size}");

        // Bounds check before flat tensor indexing
        let expected_len = seq_len * hidden_size;
        anyhow::ensure!(data.len() >= expected_len, "unexpected output tensor size");

        // Mean pooling: average token embeddings, masked by attention mask
        let mut pooled = vec![0.0f32; hidden_size];

        let mask_sum: f32 = tokens.attention_mask.iter().map(|&m| m as f32).sum();
        if mask_sum == 0.0 {
            anyhow::bail!("attention mask is all zeros");
        }

        for token_idx in 0..seq_len {
            let mask_val = tokens.attention_mask[token_idx] as f32;
            if mask_val > 0.0 {
                let offset = token_idx * hidden_size;
                for dim in 0..hidden_size {
                    pooled[dim] += data[offset + dim] * mask_val;
                }
            }
        }

        for val in &mut pooled {
            *val /= mask_sum;
        }

        // L2 normalize
        let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for val in &mut pooled {
                *val /= norm;
            }
        }

        Ok(pooled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::download::default_model_dir;

    fn load_model() -> Option<EmbeddingModel> {
        let model_dir = default_model_dir().ok()?;
        if !model_dir.join("model.onnx").exists() || !model_dir.join("tokenizer.json").exists() {
            eprintln!("Skipping model test: model not downloaded");
            return None;
        }
        EmbeddingModel::load(&model_dir).ok()
    }

    #[test]
    fn test_embed_dimension() {
        let Some(mut model) = load_model() else { return };
        let embedding = model.embed("Hello world").unwrap();
        assert_eq!(embedding.len(), 384);
    }

    #[test]
    fn test_embed_normalized() {
        let Some(mut model) = load_model() else { return };
        let embedding = model.embed("Test normalization").unwrap();
        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2 norm should be ~1.0, got {norm}");
    }

    #[test]
    fn test_similar_texts_high_similarity() {
        let Some(mut model) = load_model() else { return };

        let e1 = model.embed("The cat sat on the mat").unwrap();
        let e2 = model.embed("A cat was sitting on the mat").unwrap();

        let similarity = cosine_similarity(&e1, &e2);
        assert!(
            similarity > 0.8,
            "Similar texts should have cosine similarity > 0.8, got {similarity}"
        );
    }

    #[test]
    fn test_different_texts_lower_similarity() {
        let Some(mut model) = load_model() else { return };

        let e1 = model.embed("The cat sat on the mat").unwrap();
        let e2 = model.embed("Quantum computing uses qubits for parallel computation").unwrap();

        let similarity = cosine_similarity(&e1, &e2);
        assert!(
            similarity < 0.5,
            "Different texts should have cosine similarity < 0.5, got {similarity}"
        );
    }

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (norm_a * norm_b)
    }
}
