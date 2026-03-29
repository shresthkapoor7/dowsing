// browser.rs — Chrome control via chromiumoxide (Chrome DevTools Protocol)
//
// Uses the user's RUNNING browser — no new windows, no profile copying.
// Opens tabs in the existing session (authenticated pages just work),
// closes only the tabs it opened when done.
//
// If the browser isn't running with remote debugging, restarts it with
// --remote-debugging-port=9222. Brave/Chrome restore all tabs on restart.

use anyhow::{Context, Result};
use chromiumoxide::Browser;
use futures::StreamExt;
use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Browser detection
// ---------------------------------------------------------------------------

/// Known Chromium bundle IDs as a fast path. If the default browser isn't
/// in this list, we fall back to checking for chrome_*.pak framework files.
#[cfg(target_os = "macos")]
const CHROMIUM_BUNDLE_PREFIXES: &[&str] = &[
    "com.google.chrome",
    "com.brave.browser",
    "company.thebrowser.browser", // Arc
    "com.microsoft.edgemac",
    "org.chromium.chromium",
    "com.operasoftware.opera",
    "com.vivaldi.vivaldi",
    "net.imput.helium",
];

pub fn find_chromium_binary() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return find_chromium_binary_macos();

    #[cfg(target_os = "linux")]
    return find_chromium_binary_linux();

    #[cfg(target_os = "windows")]
    anyhow::bail!("Windows browser detection not yet implemented");
}

#[cfg(target_os = "macos")]
fn find_chromium_binary_macos() -> Result<PathBuf> {
    let bundle_id = default_browser_bundle_id_macos()
        .context("could not read default browser from LaunchServices")?;

    let bundle_id_lower = bundle_id.to_lowercase();
    let known = CHROMIUM_BUNDLE_PREFIXES
        .iter()
        .any(|prefix| bundle_id_lower.starts_with(prefix));

    // Fast path: known Chromium browser
    if known {
        return executable_for_bundle_macos(&bundle_id)
            .with_context(|| format!("found bundle {} but could not locate its executable", bundle_id));
    }

    // Slow path: check if the app has Chromium framework files (chrome_*.pak)
    // This catches any Chromium-based browser we don't know about yet
    if let Ok(exe) = executable_for_bundle_macos(&bundle_id) {
        if is_chromium_app_macos(&exe) {
            return Ok(exe);
        }
    }

    anyhow::bail!(
        "your default browser (bundle ID: {}) does not appear to be Chromium-based.\n\
         This tool requires a Chromium-based browser (Chrome, Brave, Arc, Edge, Helium, etc.).\n\
         Change your default browser in System Settings → Desktop & Dock → Default web browser.",
        bundle_id
    )
}

#[cfg(target_os = "macos")]
fn default_browser_bundle_id_macos() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let plist = format!(
        "{}/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist",
        home
    );

    let output = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", &plist])
        .output()
        .context("failed to run plutil — is macOS developer tools installed?")?;

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("plutil output was not valid JSON")?;

    let handlers = json["LSHandlers"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("LSHandlers key not found in plist"))?;

    for handler in handlers {
        if handler["LSHandlerURLScheme"].as_str() == Some("https") {
            if let Some(bundle_id) = handler["LSHandlerRoleAll"].as_str() {
                return Ok(bundle_id.to_string());
            }
        }
    }

    anyhow::bail!("https handler not found in LaunchServices plist")
}

#[cfg(target_os = "macos")]
fn executable_for_bundle_macos(bundle_id: &str) -> Result<PathBuf> {
    let bundle_id_lower = bundle_id.to_lowercase();

    let apps_dir = std::fs::read_dir("/Applications")
        .context("could not read /Applications")?;

    for entry in apps_dir.flatten() {
        let app_path = entry.path();
        if app_path.extension().and_then(|e| e.to_str()) != Some("app") {
            continue;
        }

        let info_plist = app_path.join("Contents/Info.plist");
        if !info_plist.exists() {
            continue;
        }

        let id_output = Command::new("defaults")
            .arg("read")
            .arg(app_path.join("Contents/Info").to_str().unwrap_or(""))
            .arg("CFBundleIdentifier")
            .output();

        let Ok(id_output) = id_output else { continue };
        let app_bundle_id = String::from_utf8_lossy(&id_output.stdout)
            .trim()
            .to_lowercase();

        if app_bundle_id != bundle_id_lower {
            continue;
        }

        let exe_output = Command::new("defaults")
            .arg("read")
            .arg(app_path.join("Contents/Info").to_str().unwrap_or(""))
            .arg("CFBundleExecutable")
            .output()
            .context("failed to read CFBundleExecutable")?;

        let exe_name = String::from_utf8_lossy(&exe_output.stdout)
            .trim()
            .to_string();

        let exe_path = app_path.join("Contents/MacOS").join(&exe_name);
        if exe_path.exists() {
            return Ok(exe_path);
        }
    }

    anyhow::bail!("no .app found for bundle ID {}", bundle_id)
}

/// Check if a macOS app is Chromium-based by looking for chrome_*.pak files
/// in its Frameworks directory. Every Chromium-based browser ships these.
#[cfg(target_os = "macos")]
fn is_chromium_app_macos(binary: &std::path::Path) -> bool {
    // Walk up from binary to find the .app bundle
    let path_str = binary.to_string_lossy();
    let app_end = match path_str.find(".app/") {
        Some(pos) => pos + 4, // include ".app"
        None => return false,
    };
    let app_path = std::path::Path::new(&path_str[..app_end]);
    let frameworks = app_path.join("Contents/Frameworks");
    if let Ok(entries) = std::fs::read_dir(&frameworks) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Look inside *.framework/Resources/ for chrome_*.pak
            if path.extension().and_then(|e| e.to_str()) == Some("framework") {
                let resources = path.join("Resources");
                if let Ok(res_entries) = std::fs::read_dir(&resources) {
                    for res in res_entries.flatten() {
                        let name = res.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("chrome_") && name.ends_with(".pak") {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn find_chromium_binary_linux() -> Result<PathBuf> {
    let output = Command::new("xdg-settings")
        .arg("get")
        .arg("default-web-browser")
        .output()
        .context("failed to run xdg-settings — is xdg-utils installed?")?;

    let desktop_entry = String::from_utf8(output.stdout)
        .context("non-UTF8 xdg-settings output")?
        .trim()
        .to_string();

    let bin_name = desktop_entry.trim_end_matches(".desktop");

    which::which(bin_name)
        .with_context(|| format!("default browser '{}' not found on PATH", bin_name))
}

// ---------------------------------------------------------------------------
// Connect to user's running browser
// ---------------------------------------------------------------------------

const DEBUG_PORT: u16 = 9222;

/// Connect to the user's running browser. If it doesn't have remote debugging
/// enabled, restart it with the flag (all tabs restore automatically).
pub async fn get_browser(binary: &std::path::Path) -> Result<BrowserSession> {
    // Try connecting to an already-debuggable browser
    if let Ok(session) = try_connect(DEBUG_PORT).await {
        println!("[browser] connected to running browser");
        return Ok(session);
    }

    // Check DevToolsActivePort in case it's on a different port
    if let Some(port) = find_debug_port(binary) {
        if let Ok(session) = try_connect(port).await {
            println!("[browser] connected on port {}", port);
            return Ok(session);
        }
    }

    // Browser isn't debuggable — restart it with remote debugging
    println!("[browser] restarting with remote debugging...");
    restart_with_debugging(binary).await?;

    // Connect to the restarted browser
    try_connect(DEBUG_PORT)
        .await
        .context("failed to connect after restart — is the browser running?")
}

/// Extract the macOS app name from the binary path.
/// e.g. "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser" → "Brave Browser"
#[cfg(target_os = "macos")]
fn app_name(binary: &std::path::Path) -> String {
    // Walk up to find the .app bundle and extract its name
    let path_str = binary.to_string_lossy();
    if let Some(start) = path_str.find("/Applications/") {
        let after = &path_str[start + "/Applications/".len()..];
        if let Some(end) = after.find(".app") {
            return after[..end].to_string();
        }
    }
    // Fallback: use the binary filename
    binary
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Google Chrome".to_string())
}

/// Gracefully quit the running browser and relaunch with --remote-debugging-port.
/// Brave/Chrome restore all tabs on restart.
#[cfg(target_os = "macos")]
async fn restart_with_debugging(binary: &std::path::Path) -> Result<()> {
    let name = app_name(binary);

    // Gracefully quit via AppleScript (preserves session for tab restore)
    let quit_result = Command::new("osascript")
        .arg("-e")
        .arg(format!("tell application \"{}\" to quit", name))
        .output();

    if quit_result.is_err() {
        anyhow::bail!("failed to quit {} — is it running?", name);
    }

    // Wait for it to fully shut down
    println!("[browser] waiting for {} to quit...", name);
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Check if the process is gone
        let check = Command::new("pgrep")
            .arg("-f")
            .arg(&name)
            .output();
        if let Ok(output) = check {
            if output.stdout.is_empty() {
                break;
            }
        }
    }

    // Relaunch with remote debugging
    println!("[browser] relaunching {} with remote debugging...", name);
    Command::new("open")
        .arg("-a")
        .arg(&name)
        .arg("--args")
        .arg(format!("--remote-debugging-port={}", DEBUG_PORT))
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--remote-allow-origins=*")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--disable-infobars")
        .spawn()
        .context("failed to relaunch browser")?;

    // Wait for it to start and listen on the debug port.
    // We only probe with an HTTP check, NOT a full Browser::connect,
    // to avoid leaking a websocket connection + handler task.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if debug_port_responding(DEBUG_PORT).await {
            return Ok(());
        }
    }

    anyhow::bail!("{} restarted but debug port {} not responding", name, DEBUG_PORT)
}

#[cfg(target_os = "linux")]
async fn restart_with_debugging(binary: &std::path::Path) -> Result<()> {
    // Kill existing browser
    let _ = Command::new("pkill")
        .arg("-f")
        .arg(binary.to_string_lossy().as_ref())
        .output();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Relaunch with debug port
    Command::new(binary)
        .arg(format!("--remote-debugging-port={}", DEBUG_PORT))
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--remote-allow-origins=*")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--disable-infobars")
        .spawn()
        .context("failed to relaunch browser")?;

    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if debug_port_responding(DEBUG_PORT).await {
            return Ok(());
        }
    }

    anyhow::bail!("browser restarted but debug port {} not responding", DEBUG_PORT)
}

/// Lightweight probe: just check if the debug port's HTTP endpoint responds.
/// Does NOT open a websocket connection (avoids leaking handler tasks).
async fn debug_port_responding(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/json/version", port);
    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(1))
        .send()
        .await
        .is_ok()
}

/// Check the browser's user data dir for DevToolsActivePort.
fn find_debug_port(binary: &std::path::Path) -> Option<u16> {
    let home = dirs::home_dir()?;
    let bin_lower = binary.to_string_lossy().to_lowercase();

    #[cfg(target_os = "macos")]
    let data_dir = if bin_lower.contains("brave") {
        home.join("Library/Application Support/BraveSoftware/Brave-Browser")
    } else if bin_lower.contains("arc") {
        home.join("Library/Application Support/Arc/User Data")
    } else if bin_lower.contains("microsoft edge") || bin_lower.contains("msedge") {
        home.join("Library/Application Support/Microsoft Edge")
    } else if bin_lower.contains("google chrome") || bin_lower.contains("google/chrome") {
        home.join("Library/Application Support/Google/Chrome")
    } else {
        // Unknown Chromium variant (e.g. Helium) — no known data dir mapping,
        // so don't guess Chrome's path or we'd read Chrome's DevToolsActivePort.
        return None;
    };

    #[cfg(target_os = "linux")]
    let data_dir = if bin_lower.contains("brave") {
        home.join(".config/BraveSoftware/Brave-Browser")
    } else if bin_lower.contains("google-chrome") || bin_lower.contains("google/chrome") {
        home.join(".config/google-chrome")
    } else {
        return None;
    };

    #[cfg(target_os = "windows")]
    let data_dir = if bin_lower.contains("google") && bin_lower.contains("chrome") {
        home.join(r"AppData\Local\Google\Chrome\User Data")
    } else {
        return None;
    };

    let port_file = data_dir.join("DevToolsActivePort");
    let contents = std::fs::read_to_string(port_file).ok()?;
    contents.lines().next()?.trim().parse::<u16>().ok()
}

/// A connected browser session that can disconnect without closing the browser.
///
/// The critical problem: chromiumoxide's `Browser::close()` sends the CDP
/// `Browser.close` command which tells Chrome to shut down entirely.
/// And when the process exits, tokio aborts the handler task, which drops
/// the websocket abruptly (TCP RST, no close frame). Some Chromium-based
/// browsers interpret an abrupt DevTools websocket disconnect as a signal
/// to shut down.
///
/// The solution: abort the handler task explicitly before process exit.
/// This drops the websocket connection. The browser stays alive because
/// we never sent `Browser.close` — we just disconnected the DevTools client.
pub struct BrowserSession {
    pub browser: Browser,
    handler_handle: tokio::task::JoinHandle<()>,
}

impl BrowserSession {
    /// Disconnect from the browser without closing it.
    ///
    /// 1. Drops the `Browser` struct (closes the channel to the handler).
    ///    The `Browser::drop` impl is a no-op for `connect()`-created browsers.
    /// 2. Aborts the handler task, which drops the websocket.
    ///
    /// We intentionally never call `browser.close()` — that sends the CDP
    /// `Browser.close` command which kills the browser process.
    pub async fn disconnect(self) {
        // Drop the browser — closes the mpsc channel to the handler.
        // For connect()-created browsers, Browser::drop is a no-op (no child process).
        drop(self.browser);

        // Abort the handler task. This drops the Handler and its Connection,
        // which drops the WebSocketStream. The browser stays alive because
        // we never sent the Browser.close CDP command.
        self.handler_handle.abort();
        let _ = self.handler_handle.await;
    }
}

/// Try to connect to a browser on the given port via CDP.
async fn try_connect(port: u16) -> Result<BrowserSession> {
    let url = format!("http://127.0.0.1:{}/json/version", port);
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(1))
        .send()
        .await
        .context("no browser")?;
    let text = resp.text().await?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let ws_url = json["webSocketDebuggerUrl"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no webSocketDebuggerUrl"))?;

    let (browser, mut handler) = Browser::connect(ws_url).await?;
    let handler_handle = tokio::spawn(async move {
        while handler.next().await.is_some() {}
    });
    Ok(BrowserSession {
        browser,
        handler_handle,
    })
}

// ---------------------------------------------------------------------------
// Page operations
// ---------------------------------------------------------------------------

pub type OpenedPageTracker = Arc<Mutex<HashSet<String>>>;

/// Track only the tabs this process explicitly opens.
pub fn new_opened_page_tracker() -> OpenedPageTracker {
    Arc::new(Mutex::new(HashSet::new()))
}

/// Close only the tabs this process explicitly opened.
/// Leaves the user's existing tabs untouched even if the browser's initial
/// page enumeration was incomplete.
pub async fn close_our_pages(browser: &Browser, opened_pages: &OpenedPageTracker) {
    let opened_ids: HashSet<String> = {
        let guard = opened_pages.lock().await;
        guard.iter().cloned().collect()
    };

    if let Ok(pages) = browser.pages().await {
        for page in pages {
            let id = page.target_id().as_ref().to_owned();
            if opened_ids.contains(&id) {
                let _ = page.close().await;
            }
        }
    }
}

/// Load a URL in a new tab and return the page HTML.
///
/// Waits for initial navigation, then polls the DOM until it stabilizes.
/// SPAs like LinkedIn render content via JS after the initial load event —
/// without this wait, we'd get skeleton/shimmer HTML with zero content.
pub async fn fetch_page(
    browser: &Browser,
    url: &str,
    opened_pages: &OpenedPageTracker,
) -> Result<String> {
    let page = browser.new_page(url).await?;
    opened_pages
        .lock()
        .await
        .insert(page.target_id().as_ref().to_owned());
    page.wait_for_navigation().await?;

    // Wait for the page to settle — poll DOM until HTML stabilizes exactly
    let mut last_html: Option<String> = None;
    let mut stable_count = 0u8;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let html = page.content().await?;
        if !html.is_empty() && last_html.as_deref() == Some(&html) {
            stable_count += 1;
            if stable_count >= 2 {
                return Ok(html);
            }
        } else {
            stable_count = 0;
        }
        last_html = Some(html);
    }

    // Fallback: return whatever we have after the wait
    let html = page.content().await?;
    Ok(html)
}
