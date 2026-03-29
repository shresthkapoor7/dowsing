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
use std::time::Instant;

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

    let total_start = Instant::now();

    // --- Embedder ---
    println!("  Loading model...");
    let mut embedder = embedder::Embedder::new("models/model.onnx", "models/tokenizer.json")?;
    let query_embedding = embedder.embed(&cli.query)?;

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
        println!("  Debug logs → debug_logs/");
    }

    // --- Navigate ---
    println!("  Navigating...");
    let nav_start = Instant::now();
    let result = navigator::navigate(
        &query_embedding,
        &cli.start,
        &session.browser,
        &opened_pages,
        &mut embedder,
        cli.debug,
    )
    .await?;
    let nav_elapsed = nav_start.elapsed();

    // --- Clean up our tabs (leave the user's tabs alone) ---
    browser::close_our_pages(&session.browser, &opened_pages).await;

    // --- Disconnect from browser without closing it ---
    session.disconnect().await;

    let total_elapsed = total_start.elapsed();

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

    let clipboard_ok = match Clipboard::new().and_then(|mut cb| cb.set_text(clipboard_text.clone())) {
        Ok(_) => true,
        Err(_) => {
            // OSC 52 fallback for remote/SSH terminals
            use base64::Engine;
            use std::io::Write;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&clipboard_text);
            print!("\x1B]52;c;{}\x07", encoded);
            let _ = std::io::stdout().flush();
            false
        }
    };

    // --- Summary ---
    println!("  Done. Paste into your LLM of choice.\n");

    println!("Results:");
    println!("  Pages found:  {}", page_count);
    println!("  Total tokens: {}", token_count);
    println!("  Hops taken:   {}", result.hops);
    println!("  Nav time:     {:.1}s", nav_elapsed.as_secs_f64());
    println!("  Total time:   {:.1}s", total_elapsed.as_secs_f64());

    if page_count > 0 {
        println!("\nPages by relevance:");
        for (i, page) in result.pages.iter().enumerate() {
            println!(
                "  {}. {:.4}  {}",
                i + 1,
                page.score,
                page.url,
            );
        }
    }

    if clipboard_ok {
        println!("\nCopied to clipboard.");
    } else {
        println!("\nCopied to clipboard via terminal (OSC 52).");
    }

    Ok(())
}
