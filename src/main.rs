// main.rs — CLI entry point
//
// Phase 1: exercises the two core primitives independently.
//   1. browser::fetch_html   — launches a Chromium browser, loads a URL, returns raw HTML
//   2. embedder::Embedder    — loads ONNX model, embeds a string, returns Vec<f32>
//
// Output: plain text of the best page found, copied to clipboard.
//         Paste into any LLM of your choice.
//
// Run (Phase 1):
//   cargo run --bin semantic-navigator -- --query "your question" --start "https://example.com"
//
// Phase 1 limitation: copies raw HTML with tags stripped via whitespace collapse.
// Phase 2 replaces this with proper Readability-style extraction (clean prose only).

mod browser;
mod embedder;

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

    // --- Primitive 1: browser ---
    // Print which binary was found so you can confirm it's the browser you expect.
    // Profile: Phase 1 uses a FRESH temporary profile (no cookies, no sessions).
    //          Phase 3 will add user_data_dir() to inherit your existing sessions.
    let binary = browser::find_chromium_binary()?;
    println!("[browser] using binary: {}", binary.display());
    println!("[browser] profile: temporary (no cookies) — Phase 3 adds your real profile");
    println!("[browser] fetching {}", cli.start);
    // Pass the resolved binary in so fetch_html doesn't scan /Applications a second time.
    let html = browser::fetch_html(&cli.start, &binary).await?;
    println!("[browser] fetched {} bytes", html.len());

    // --- Primitive 2: embedder ---
    println!("[embedder] loading model from models/");
    let mut embedder = embedder::Embedder::new("models/model.onnx", "models/tokenizer.json")?;

    let query_embedding = embedder.embed(&cli.query)?;
    println!(
        "[embedder] embedded query ({} dims): {:?}",
        query_embedding.len(),
        &query_embedding[..4]   // print first 4 dims to keep output readable
    );

    // Strip HTML tags with a simple whitespace collapse — good enough for Phase 1.
    // Phase 2 replaces this with proper Readability extraction (clean prose, no nav/footer).
    let plain_text = strip_tags(&html);

    // Score the full plain text against the query so the number is meaningful.
    let page_embedding = embedder.embed(&plain_text)?;
    let score: f32 = query_embedding
        .iter()
        .zip(page_embedding.iter())
        .map(|(a, b)| a * b)
        .sum();

    println!("[score] page vs query: {:.4}", score);
    println!("[output] {} chars of plain text", plain_text.len());

    // Copy to clipboard — paste into any LLM you want.
    let mut clipboard = Clipboard::new()?;
    clipboard.set_text(plain_text)?;
    println!("[clipboard] copied — paste into your LLM");

    Ok(())
}

/// Collapse HTML into plain text by stripping tags and normalizing whitespace.
/// Skips the full contents of <script> and <style> blocks so JS and CSS
/// don't end up in the clipboard output.
/// Phase 2 replaces this with proper Readability-style extraction.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip_block: Option<&'static str> = None; // closing tag we're waiting for
    let mut tag_buf = String::new(); // accumulates the current tag name

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag_buf.clear();
            }
            '>' => {
                in_tag = false;
                // Decide whether to enter or leave a skip block based on the tag name.
                // tag_buf may look like "script", "/script", "style src=...", etc.
                let name: String = tag_buf
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_alphanumeric())
                    .flat_map(|c| c.to_lowercase())
                    .collect();

                match skip_block {
                    None if name == "script" => skip_block = Some("</script>"),
                    None if name == "style" => skip_block = Some("</style>"),
                    Some(closing) => {
                        let closing_name: String = closing
                            .trim_start_matches('<')
                            .trim_start_matches('/')
                            .trim_end_matches('>')
                            .to_string();
                        if name == closing_name {
                            skip_block = None;
                        }
                    }
                    _ => {}
                }
            }
            _ if in_tag => tag_buf.push(ch),
            _ if skip_block.is_none() => out.push(ch),
            _ => {}
        }
    }
    // Collapse runs of whitespace into single spaces
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
