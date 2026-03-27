// main.rs — CLI entry point
//
// Connects to the user's running browser, navigates using embedding
// similarity, copies all relevant pages to clipboard.

mod browser;
mod embedder;
mod extractor;
mod navigator;

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

    url::Url::parse(&cli.start)
        .map_err(|e| anyhow::anyhow!("invalid --start URL '{}': {}", cli.start, e))?;

    // --- Embedder ---
    println!("[embedder] loading model...");
    let mut embedder = embedder::Embedder::new("models/model.onnx", "models/tokenizer.json")?;
    let query_embedding = embedder.embed(&cli.query)?;
    println!("[embedder] query embedded ({} dims)", query_embedding.len());

    // --- Browser: connect to running browser ---
    let binary = browser::find_chromium_binary()?;
    let session = browser::get_browser(&binary).await?;
    let opened_pages = browser::new_opened_page_tracker();

    // --- Navigate ---
    println!("[nav] starting from {}", cli.start);
    let result = navigator::navigate(
        &query_embedding,
        &cli.start,
        &session.browser,
        &opened_pages,
        &mut embedder,
    )
    .await?;

    // --- Clean up our tabs (leave the user's tabs alone) ---
    browser::close_our_pages(&session.browser, &opened_pages).await;

    // --- Disconnect from browser without closing it ---
    session.disconnect().await;

    // --- Results ---
    println!("\n--- Results (sorted by relevance) ---");
    for (i, page) in result.pages.iter().enumerate() {
        let preview: String = page.content.chars().take(80).collect();
        println!(
            "  {}. {:.4}  (hop {})  {}  [{}...]",
            i + 1,
            page.score,
            page.hop,
            page.url,
            preview
        );
    }

    // --- Clipboard: ALL pages, most relevant first ---
    let mut clipboard_text = String::new();
    for (i, page) in result.pages.iter().enumerate() {
        clipboard_text.push_str(&format!(
            "=== Page {} (score: {:.4}) ===\nURL: {}\n\n{}\n\n",
            i + 1,
            page.score,
            page.url,
            page.content
        ));
    }

    let page_count = result.pages.len();
    let char_count = clipboard_text.len();
    let mut clipboard = Clipboard::new()?;
    clipboard.set_text(clipboard_text)?;
    println!(
        "\n[clipboard] {} page(s) copied ({} chars) — paste into your LLM",
        page_count, char_count
    );

    Ok(())
}
