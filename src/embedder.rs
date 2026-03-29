// embedder.rs — ONNX-based text embedding using all-MiniLM-L6-v2
//
// Supports single and batch embedding with an in-memory cache.
//
// Model: all-MiniLM-L6-v2 exported to ONNX. 23 MB. 384 output dimensions.
// Download with: cargo run --bin download-model
//
// Pipeline per text:
//   1. Tokenize with HuggingFace tokenizer → input_ids, attention_mask, token_type_ids
//   2. Run ONNX session → last_hidden_state of shape [batch, seq_len, 384]
//   3. Mean pool over the seq_len dimension (masked) → [384]
//   4. L2 normalize → unit vector, so dot product == cosine similarity
//
// Batch embedding pads all sequences to the longest in the batch so ONNX
// processes them in a single forward pass. The attention mask ensures padded
// tokens don't contribute to the mean pool.
//
// Embedding cache: keyed by hash of the (truncated) input text. Nav links
// like "Home", "Docs", "API" appear on every page with identical context
// strings — caching avoids re-embedding them each hop.

use anyhow::{Context, Result};
use ndarray::Array2;
use ort::{session::Session, value::Tensor};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tokenizers::Tokenizer;

const HIDDEN_SIZE: usize = 384;
const MAX_CHARS: usize = 1800; // ~512 tokens at ~3.5 chars/token average

pub struct Embedder {
    session: Session,
    tokenizer: Tokenizer,
    cache: HashMap<u64, (String, Vec<f32>)>,
}

impl Embedder {
    /// Load the ONNX model and tokenizer from disk.
    pub fn new(model_path: &str, tokenizer_path: &str) -> Result<Self> {
        let session = Session::builder()
            .context("failed to create ORT session builder")?
            .commit_from_file(model_path)
            .with_context(|| format!("failed to load ONNX model from {model_path}"))?;

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer from {tokenizer_path}: {e}"))?;

        Ok(Self {
            session,
            tokenizer,
            cache: HashMap::new(),
        })
    }

    /// Embed a single text and return a L2-normalized [384] vector.
    /// Results are cached by text content.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed_batch(&[text])?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("embed_batch returned empty results"))
    }

    /// Embed multiple texts in a single ONNX forward pass.
    /// Cached texts are skipped — only uncached texts hit the model.
    /// Returns embeddings in the same order as the input texts.
    pub fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        anyhow::ensure!(!texts.is_empty(), "cannot embed empty batch");

        // Truncate and compute cache keys for all texts
        let truncated: Vec<&str> = texts
            .iter()
            .map(|t| {
                let t = t.trim();
                if t.len() > MAX_CHARS {
                    // Find a safe byte boundary to avoid splitting multi-byte UTF-8
                    let safe_end = t
                        .char_indices()
                        .nth(MAX_CHARS)
                        .map(|(i, _)| i)
                        .unwrap_or(t.len());
                    &t[..safe_end]
                } else {
                    t
                }
            })
            .collect();

        let keys: Vec<u64> = truncated.iter().map(|t| hash_text(t)).collect();

        // Find which indices need embedding (not in cache or hash collision)
        let uncached: Vec<usize> = keys
            .iter()
            .zip(truncated.iter())
            .enumerate()
            .filter(|(_, (k, text))| {
                match self.cache.get(k) {
                    Some((cached_text, _)) => cached_text != *text, // hash collision
                    None => true,
                }
            })
            .map(|(i, _)| i)
            .collect();

        // Run ONNX only for uncached texts
        if !uncached.is_empty() {
            let uncached_texts: Vec<&str> = uncached.iter().map(|&i| truncated[i]).collect();
            let embeddings = self.run_batch(&uncached_texts)?;

            for (idx, emb) in uncached.into_iter().zip(embeddings) {
                self.cache.insert(keys[idx], (truncated[idx].to_owned(), emb));
            }
        }

        // Collect results in input order from cache
        let results: Vec<Vec<f32>> = keys
            .iter()
            .map(|k| self.cache[k].1.clone())
            .collect();

        Ok(results)
    }

    /// Return the number of cached embeddings.
    #[allow(dead_code)]
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Run ONNX inference on a batch of (already truncated) texts.
    /// Pads all sequences to the max length in the batch.
    fn run_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Tokenize all texts
        let encodings: Vec<_> = texts
            .iter()
            .map(|t| {
                self.tokenizer
                    .encode(*t, true)
                    .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;

        let empty_indices: Vec<usize> = encodings
            .iter()
            .enumerate()
            .filter(|(_, e)| e.get_ids().is_empty())
            .map(|(i, _)| i)
            .collect();
        anyhow::ensure!(
            empty_indices.is_empty(),
            "tokenizer produced empty encoding for inputs at indices: {:?}",
            empty_indices
        );

        let batch_size = encodings.len();
        let max_seq_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap();

        // Build padded [batch_size, max_seq_len] arrays
        let mut all_ids = vec![0i64; batch_size * max_seq_len];
        let mut all_mask = vec![0i64; batch_size * max_seq_len];
        let mut all_type_ids = vec![0i64; batch_size * max_seq_len];
        let mut seq_lens = Vec::with_capacity(batch_size);

        for (i, enc) in encodings.iter().enumerate() {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let tids = enc.get_type_ids();
            let slen = ids.len();
            seq_lens.push(slen);

            let offset = i * max_seq_len;
            for j in 0..slen {
                all_ids[offset + j] = ids[j] as i64;
                all_mask[offset + j] = mask[j] as i64;
                all_type_ids[offset + j] = tids[j] as i64;
            }
            // Remaining positions stay 0 (pad token, mask=0)
        }

        let input_ids = Array2::from_shape_vec((batch_size, max_seq_len), all_ids)
            .context("failed to shape input_ids")?;
        let attention_mask = Array2::from_shape_vec((batch_size, max_seq_len), all_mask)
            .context("failed to shape attention_mask")?;
        let token_type_ids = Array2::from_shape_vec((batch_size, max_seq_len), all_type_ids)
            .context("failed to shape token_type_ids")?;

        let input_ids_t =
            Tensor::<i64>::from_array(input_ids).context("failed to create input_ids tensor")?;
        let attention_mask_t = Tensor::<i64>::from_array(attention_mask)
            .context("failed to create attention_mask tensor")?;
        let token_type_ids_t = Tensor::<i64>::from_array(token_type_ids)
            .context("failed to create token_type_ids tensor")?;

        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids"      => input_ids_t,
                "attention_mask" => attention_mask_t,
                "token_type_ids" => token_type_ids_t,
            ])
            .context("ONNX inference failed")?;

        let hidden = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .context("failed to extract last_hidden_state")?;

        // Mean pool each item in the batch (masked — only real tokens)
        let mut results = Vec::with_capacity(batch_size);
        for i in 0..batch_size {
            let slen = seq_lens[i];
            let mut pooled = vec![0f32; HIDDEN_SIZE];
            for token_idx in 0..slen {
                for dim in 0..HIDDEN_SIZE {
                    pooled[dim] += hidden[[i, token_idx, dim]];
                }
            }
            let n = slen as f32;
            for x in pooled.iter_mut() {
                *x /= n;
            }

            // L2 normalize
            let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 1e-9 {
                for x in pooled.iter_mut() {
                    *x /= norm;
                }
            }

            results.push(pooled);
        }

        Ok(results)
    }
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
