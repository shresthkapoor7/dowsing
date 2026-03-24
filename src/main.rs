// main.rs — CLI entry point
//
// Phase 2: end-to-end integration.
//   - Fetches page HTML via chromiumoxide (browser.rs)
//   - Extracts clean prose text with Readability-style heuristics (extractor.rs)
//   - Extracts links with rich context strings (extractor.rs)
//   - Embeds query + page content + every link context (embedder.rs)
//   - Prints page score and top-10 scored links to stdout
//   - Copies page content to clipboard
//
// Done when: `cargo run -- --query "..." --start "https://..."` prints a page
// score and a ranked list of links.

mod browser;
mod embedder;
mod extractor;

use anyhow::Result;
use arboard::Clipboard;
use clap::Parser;

#[derive(Parser)]
#[command(name = "semantic-navigator")]
struct Cli {
    /// The question to answer / page to find
    #[arg(long)]
    query: String,

    /// Starting URL
    #[arg(long)]
    start: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // --- Browser: fetch raw HTML ---
    let binary = browser::find_chromium_binary()?;
    println!("[browser] using binary: {}", binary.display());
    println!("[browser] fetching {}", cli.start);
    let html = browser::fetch_html(&cli.start, &binary).await?;
    println!("[browser] fetched {} bytes of HTML", html.len());

    // --- Embedder: load model, embed query once ---
    println!("[embedder] loading model from models/");
    let mut embedder = embedder::Embedder::new("models/model.onnx", "models/tokenizer.json")?;
    let query_embedding = embedder.embed(&cli.query)?;
    println!(
        "[embedder] query embedded ({} dims): [{:.4}, {:.4}, {:.4}, {:.4}, ...]",
        query_embedding.len(),
        query_embedding[0],
        query_embedding[1],
        query_embedding[2],
        query_embedding[3],
    );

    // --- Extractor: clean prose from the page ---
    let page_content = extractor::extract_page_content(&html);
    println!(
        "[extractor] extracted {} chars of clean content",
        page_content.len()
    );

    // --- Score: how relevant is this page to the query? ---
    let page_embedding = embedder.embed(&page_content)?;
    let page_score: f32 = dot(&query_embedding, &page_embedding);
    println!("[score] page relevance: {:.4}", page_score);

    // --- Links: extract + score every link on the page ---
    let links = extractor::extract_links(&html, &cli.start);
    println!("[extractor] found {} links", links.len());

    let mut scored: Vec<(f32, &str, &str)> = links
        .iter()
        .filter_map(|link| {
            // Embed the context string; skip on error (malformed text etc.)
            embedder
                .embed(&link.context_string)
                .ok()
                .map(|emb| (dot(&query_embedding, &emb), link.url.as_str(), link.context_string.as_str()))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    println!("\n--- Top 10 links by relevance ---");
    for (score, url, ctx) in scored.iter().take(10) {
        // Print a truncated context string so it fits on one line
        let preview: String = ctx.chars().take(100).collect();
        println!("  {:.4}  {}  [{}...]", score, url, preview);
    }
    println!();

    // --- Clipboard: copy page content for pasting into any LLM ---
    let mut clipboard = Clipboard::new()?;
    clipboard.set_text(page_content)?;
    println!("[clipboard] page content copied — paste into your LLM");

    Ok(())
}

/// Dot product of two equal-length vectors.
/// Because embeddings are L2-normalized this equals cosine similarity.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
