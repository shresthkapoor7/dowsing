// embedder.rs — ONNX-based text embedding using all-MiniLM-L6-v2
//
// Phase 1 responsibility: load the model, embed a string, return Vec<f32>.
//
// Model: all-MiniLM-L6-v2 exported to ONNX. 23 MB. 384 output dimensions.
// Download with: cargo run --bin download-model
//
// Pipeline for a single input string:
//   1. Tokenize with HuggingFace tokenizer → input_ids, attention_mask, token_type_ids
//   2. Run ONNX session → last_hidden_state of shape [1, seq_len, 384]
//   3. Mean pool over the seq_len dimension → [384]
//   4. L2 normalize → unit vector, so dot product == cosine similarity
//
// Why L2 normalize: at search time we want cosine similarity, which normally
// requires computing two magnitudes per comparison. Normalizing upfront turns
// cosine similarity into a plain dot product, which is cheaper in the inner loop.
//
// The query embedding is computed once at startup and cached by the caller
// (see navigator.rs in Phase 3). Never re-embed the same query string.

use anyhow::{Context, Result};
use ndarray::Array2;
use ort::{session::Session, value::Tensor};
use tokenizers::Tokenizer;

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Load the ONNX model and tokenizer from disk.
    ///
    /// Paths are relative to the working directory (project root).
    ///   model_path:     "models/model.onnx"
    ///   tokenizer_path: "models/tokenizer.json"
    pub fn new(model_path: &str, tokenizer_path: &str) -> Result<Self> {
        let session = Session::builder()
            .context("failed to create ORT session builder")?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load ONNX model from {model_path}"))?;

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer from {tokenizer_path}: {e}"))?;

        Ok(Self { session, tokenizer })
    }

    /// Embed `text` and return a L2-normalized vector of 384 floats.
    ///
    /// The returned vector is a unit vector. Dot product between two of these
    /// equals cosine similarity — no magnitude computation needed at query time.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        // --- Step 1: tokenize ---
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&x| x as i64)
            .collect();
        let type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&x| x as i64)
            .collect();

        let seq_len = ids.len();

        // ONNX model expects shape [batch=1, seq_len]
        let input_ids = Array2::from_shape_vec((1, seq_len), ids)
            .context("failed to shape input_ids")?;
        let attention_mask = Array2::from_shape_vec((1, seq_len), mask)
            .context("failed to shape attention_mask")?;
        let token_type_ids = Array2::from_shape_vec((1, seq_len), type_ids)
            .context("failed to shape token_type_ids")?;

        // Wrap ndarray arrays in ort Tensor values.
        // SessionInputValue::from(Tensor<T>) is the required type for ort::inputs!
        let input_ids_t = Tensor::<i64>::from_array(input_ids)
            .context("failed to create input_ids tensor")?;
        let attention_mask_t = Tensor::<i64>::from_array(attention_mask)
            .context("failed to create attention_mask tensor")?;
        let token_type_ids_t = Tensor::<i64>::from_array(token_type_ids)
            .context("failed to create token_type_ids tensor")?;

        // --- Step 2: run ONNX inference ---
        // Output: last_hidden_state of shape [1, seq_len, 384]
        // ort::inputs! with key => value returns Vec directly (no ? needed)
        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids"      => input_ids_t,
                "attention_mask" => attention_mask_t,
                "token_type_ids" => token_type_ids_t,
            ])
            .context("ONNX inference failed")?;

        // try_extract_array returns an ndarray::ArrayViewD supporting [[batch, token, dim]] indexing
        let hidden = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .context("failed to extract last_hidden_state")?;

        // shape: [1, seq_len, 384]
        let hidden_size = 384usize;

        // --- Step 3: mean pool over token dimension ---
        let mut pooled = vec![0f32; hidden_size];
        for token_idx in 0..seq_len {
            for dim in 0..hidden_size {
                pooled[dim] += hidden[[0, token_idx, dim]];
            }
        }
        let n = seq_len as f32;
        for x in pooled.iter_mut() {
            *x /= n;
        }

        // --- Step 4: L2 normalize ---
        let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-9 {
            for x in pooled.iter_mut() {
                *x /= norm;
            }
        }

        Ok(pooled)
    }
}
