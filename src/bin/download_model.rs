// download_model.rs — fetch model files from HuggingFace Hub
//
// Downloads two files into the models/ directory (created if missing):
//   models/model.onnx       — all-MiniLM-L6-v2 ONNX export (~23 MB)
//   models/tokenizer.json   — HuggingFace tokenizer config
//
// Run once before using the embedder:
//   cargo run --bin download-model
//
// Source: sentence-transformers/all-MiniLM-L6-v2 on HuggingFace Hub.
// The ONNX variant lives under the onnx/ subdirectory of that repo.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

const MODEL_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx";

const TOKENIZER_URL: &str =
    "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/tokenizer.json";

fn download(url: &str, dest: &str) -> Result<()> {
    if Path::new(dest).exists() {
        println!("[skip] {} already exists", dest);
        return Ok(());
    }

    println!("[download] {} → {}", url, dest);
    let bytes = reqwest::blocking::get(url)
        .with_context(|| format!("GET {url} failed"))?
        .error_for_status()
        .with_context(|| format!("non-2xx response from {url}"))?
        .bytes()
        .context("failed to read response body")?;

    let mut file = fs::File::create(dest)
        .with_context(|| format!("failed to create {dest}"))?;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write {dest}"))?;

    println!("[ok] wrote {} bytes to {}", bytes.len(), dest);
    Ok(())
}

fn main() -> Result<()> {
    fs::create_dir_all("models").context("failed to create models/ directory")?;

    download(MODEL_URL, "models/model.onnx")?;
    download(TOKENIZER_URL, "models/tokenizer.json")?;

    println!("[done] models ready in models/");
    Ok(())
}
