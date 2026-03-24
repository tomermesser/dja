use anyhow::{Context, Result};
use std::path::Path;
use tokenizers::Tokenizer;

const MAX_LENGTH: usize = 256;

/// Wrapper around a HuggingFace tokenizer loaded from `tokenizer.json`.
pub struct DjaTokenizer {
    inner: Tokenizer,
}

/// Tokenization output: input IDs, attention mask, and token type IDs.
pub struct TokenizedInput {
    pub input_ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub token_type_ids: Vec<i64>,
}

impl DjaTokenizer {
    /// Load a tokenizer from a `tokenizer.json` file.
    pub fn load(model_dir: &Path) -> Result<Self> {
        let tokenizer_path = model_dir.join("tokenizer.json");
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {e}"))?;

        // Configure truncation and padding
        tokenizer.with_truncation(Some(tokenizers::TruncationParams {
            max_length: MAX_LENGTH,
            ..Default::default()
        }))
        .map_err(|e| anyhow::anyhow!("failed to set truncation: {e}"))?;

        tokenizer.with_padding(Some(tokenizers::PaddingParams {
            strategy: tokenizers::PaddingStrategy::BatchLongest,
            ..Default::default()
        }));

        Ok(Self { inner: tokenizer })
    }

    /// Tokenize a single text string.
    pub fn tokenize(&self, text: &str) -> Result<TokenizedInput> {
        let encoding = self
            .inner
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))
            .context("failed to encode text")?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let token_type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&t| t as i64)
            .collect();

        Ok(TokenizedInput {
            input_ids,
            attention_mask,
            token_type_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::download::default_model_dir;

    #[test]
    fn test_tokenizer_token_count() {
        let model_dir = default_model_dir().unwrap();
        if !model_dir.join("tokenizer.json").exists() {
            eprintln!("Skipping tokenizer test: model not downloaded");
            return;
        }

        let tokenizer = DjaTokenizer::load(&model_dir).unwrap();
        let output = tokenizer.tokenize("Hello world").unwrap();

        // "Hello world" should produce a small number of tokens (including [CLS] and [SEP])
        assert!(output.input_ids.len() >= 3);
        assert!(output.input_ids.len() <= MAX_LENGTH);
        assert_eq!(output.input_ids.len(), output.attention_mask.len());
        assert_eq!(output.input_ids.len(), output.token_type_ids.len());
    }

    #[test]
    fn test_tokenizer_truncation() {
        let model_dir = default_model_dir().unwrap();
        if !model_dir.join("tokenizer.json").exists() {
            eprintln!("Skipping tokenizer truncation test: model not downloaded");
            return;
        }

        let tokenizer = DjaTokenizer::load(&model_dir).unwrap();
        // Create a very long input
        let long_text = "word ".repeat(1000);
        let output = tokenizer.tokenize(&long_text).unwrap();

        assert!(output.input_ids.len() <= MAX_LENGTH);
    }
}
