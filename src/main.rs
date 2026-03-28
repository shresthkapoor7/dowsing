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
#[command(name = "dowsing")]
struct Cli {
    /// The question to answer / page to find
    query: String,

    /// Starting URL
    start: String,

    /// Write debug logs (raw HTML, extracted text, link contexts) to debug_logs/
    #[arg(long, default_value_t = false)]
    debug: bool,
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

    // --- Debug setup ---
    if cli.debug {
        let log_dir = std::path::Path::new("debug_logs");
        if log_dir.exists() {
            std::fs::remove_dir_all(log_dir)?;
        }
        std::fs::create_dir_all(log_dir)?;
        println!("[debug] logging to debug_logs/");
    }

    // --- Navigate ---
    println!("[nav] starting from {}", cli.start);
    let result = navigator::navigate(
        &query_embedding,
        &cli.start,
        &session.browser,
        &opened_pages,
        &mut embedder,
        cli.debug,
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
    let bpe = tiktoken_rs::get_bpe_from_model("gpt-4o")
        .map_err(|e| anyhow::anyhow!("tiktoken init failed: {}", e))?;
    let token_count = bpe.encode_with_special_tokens(&clipboard_text).len();

    match Clipboard::new().and_then(|mut cb| cb.set_text(clipboard_text.clone())) {
        Ok(_) => {
            println!(
                "\n[clipboard] {} page(s) copied (~{} tokens) — paste into your LLM",
                page_count, token_count
            );
        }
        Err(_) => {
            // OSC 52 fallback for remote/SSH terminals
            use base64::Engine;
            use std::io::Write;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&clipboard_text);
            if encoded.len() > 100_000 {
                eprintln!(
                    "\n[warning] clipboard content is large (~{}KB encoded) — some terminals may truncate OSC 52",
                    encoded.len() / 1024
                );
            }
            print!("\x1B]52;c;{}\x07", encoded);
            let _ = std::io::stdout().flush();
            println!(
                "\n[clipboard] {} page(s) copied via terminal (~{} tokens) — paste into your LLM (OSC 52, not all terminals support this)",
                page_count, token_count
            );
        }
    }

    Ok(())
}
