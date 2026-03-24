// browser.rs — Chrome control via chromiumoxide (Chrome DevTools Protocol)
//
// Phase 1 responsibility: launch a Chromium-based browser, load a URL, return raw HTML.
//
// Key decisions:
//   - CDP (Chrome DevTools Protocol) only works with Chromium-based browsers.
//     Safari and Firefox are not supported. Arc, Brave, Edge, Chromium, and
//     Google Chrome all work.
//   - find_chromium_binary() searches common install locations in priority order.
//     First match wins. No hardcoded assumption of "Google Chrome".
//   - headless: false during development so you can watch the browser.
//     Flip to true for benchmarking (see BrowserConfig::builder().headless(true)).
//   - No user profile in Phase 1. The browser opens a fresh temporary profile.
//     Phase 3 adds user_data_dir() so the navigator inherits existing cookies
//     and authenticated sessions without needing credentials.
//   - The browser handler must be spawned on a separate task. chromiumoxide
//     processes CDP events in the background; if the handler is not polled the
//     browser will hang. See the spawn call in fetch_html.
//
// User profile paths (for Phase 3, pick the one matching the browser found here):
//   Arc:    ~/Library/Application Support/Arc/User Data/Default
//   Chrome: ~/Library/Application Support/Google/Chrome/Default
//   Brave:  ~/Library/Application Support/BraveSoftware/Brave-Browser/Default
//   Edge:   ~/Library/Application Support/Microsoft Edge/Default

use anyhow::{Context, Result};
use chromiumoxide::{Browser, BrowserConfig, browser::HeadlessMode};
use futures::StreamExt;
use std::path::PathBuf;

/// Locations to search for a Chromium-based binary, in priority order.
/// Arc is listed first since it is the most common default on macOS these days.
#[cfg(target_os = "macos")]
const CHROMIUM_CANDIDATES: &[&str] = &[
    "/Applications/Arc.app/Contents/MacOS/Arc",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

#[cfg(target_os = "linux")]
const CHROMIUM_CANDIDATES: &[&str] = &[
    "google-chrome",
    "chromium",
    "chromium-browser",
    "brave-browser",
    "microsoft-edge",
];

#[cfg(target_os = "windows")]
const CHROMIUM_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
];

/// Return the path to the first Chromium-compatible binary found on this system.
pub fn find_chromium_binary() -> Result<PathBuf> {
    for candidate in CHROMIUM_CANDIDATES {
        let path = PathBuf::from(candidate);
        // Absolute path: check it exists. Bare name: trust PATH lookup.
        if path.is_absolute() {
            if path.exists() {
                return Ok(path);
            }
        } else if which::which(candidate).is_ok() {
            return Ok(PathBuf::from(candidate));
        }
    }
    anyhow::bail!(
        "no Chromium-based browser found. Install Arc, Chrome, Brave, or Edge.\n\
         Searched: {}",
        CHROMIUM_CANDIDATES.join(", ")
    )
}

/// Launch a Chromium-based browser, navigate to `url`, and return the full page HTML.
///
/// Opens a non-headless window so you can watch what happens during development.
/// Each call launches a fresh browser instance and closes it when done.
pub async fn fetch_html(url: &str) -> Result<String> {
    let binary = find_chromium_binary()
        .context("could not find a Chromium-based browser to launch")?;

    let config = BrowserConfig::builder()
        .chrome_executable(binary)
        // Suppress first-run dialogs and default-browser prompts that block CDP
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-default-apps")
        // Visible window for development. Switch to .headless(true) for benchmarking.
        .headless_mode(HeadlessMode::False)
        .build()
        .map_err(|e| anyhow::anyhow!("BrowserConfig error: {}", e))?;

    let (browser, mut handler) = Browser::launch(config).await?;

    // The handler drives the CDP event loop. Must run concurrently or all
    // browser calls will deadlock waiting for responses that never arrive.
    //
    // Do NOT break on errors — many CDP events are benign protocol noise.
    // Breaking early closes the internal oneshot channels, which surfaces
    // as "oneshot canceled" on every subsequent browser call.
    tokio::spawn(async move {
        while handler.next().await.is_some() {}
    });

    let page = browser.new_page(url).await?;

    // Wait for the page to settle before reading content.
    page.wait_for_navigation().await?;

    let html = page.content().await?;

    Ok(html)
}
