// browser.rs — Chrome control via chromiumoxide (Chrome DevTools Protocol)
//
// Phase 1 responsibility: launch a Chromium-based browser, load a URL, return raw HTML.
//
// Key decisions:
//   - CDP (Chrome DevTools Protocol) only works with Chromium-based browsers.
//     Safari and Firefox are not supported. Arc, Brave, Edge, Chromium, and
//     Google Chrome all work.
//   - find_chromium_binary() asks the OS what the default browser is, then
//     resolves its executable path. No hardcoded priority list.
//   - If the default browser is not Chromium-based (e.g. Safari, Firefox) the
//     error message says so clearly.
//   - headless(false) during development so you can watch the browser.
//     Switch to HeadlessMode::True for benchmarking.
//   - No user profile in Phase 1. The browser opens a fresh temporary profile.
//     Phase 3 adds user_data_dir() so the navigator inherits existing sessions.
//
// User profile paths (for Phase 3):
//   Arc:    ~/Library/Application Support/Arc/User Data/Default
//   Chrome: ~/Library/Application Support/Google/Chrome/Default
//   Brave:  ~/Library/Application Support/BraveSoftware/Brave-Browser/Default
//   Edge:   ~/Library/Application Support/Microsoft Edge/Default

use anyhow::{Context, Result};
use chromiumoxide::browser::HeadlessMode;
use chromiumoxide::{Browser, BrowserConfig};
use futures::StreamExt;
use std::path::PathBuf;
use std::process::Command;

// Bundle IDs of known Chromium-based browsers on macOS.
// Used to validate that the default browser supports CDP before launching.
#[cfg(target_os = "macos")]
const CHROMIUM_BUNDLE_PREFIXES: &[&str] = &[
    "com.google.chrome",
    "com.brave.browser",
    "company.thebrowser.browser", // Arc
    "com.microsoft.edgemac",
    "org.chromium.chromium",
    "com.operasoftware.opera",
    "com.vivaldi.vivaldi",
];

/// Return the path to the default browser's executable.
///
/// Asks the OS directly — macOS via LaunchServices plist + mdfind,
/// Linux via xdg-settings. Fails with a clear message if the default
/// browser is not Chromium-based (Safari, Firefox, etc.).
pub fn find_chromium_binary() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return find_chromium_binary_macos();

    #[cfg(target_os = "linux")]
    return find_chromium_binary_linux();

    #[cfg(target_os = "windows")]
    anyhow::bail!("Windows browser detection not yet implemented");
}

/// macOS: read the LaunchServices plist for the default https handler,
/// verify it is Chromium-based, then resolve its executable path.
#[cfg(target_os = "macos")]
fn find_chromium_binary_macos() -> Result<PathBuf> {
    let bundle_id = default_browser_bundle_id_macos()
        .context("could not read default browser from LaunchServices")?;

    // Verify it's a browser that supports CDP before trying to launch it.
    let bundle_id_lower = bundle_id.to_lowercase();
    let is_chromium = CHROMIUM_BUNDLE_PREFIXES
        .iter()
        .any(|prefix| bundle_id_lower.starts_with(prefix));

    if !is_chromium {
        anyhow::bail!(
            "your default browser (bundle ID: {}) does not support CDP.\n\
             This tool requires a Chromium-based browser: Arc, Chrome, Brave, or Edge.\n\
             Change your default browser in System Settings → Desktop & Dock → Default web browser.",
            bundle_id
        );
    }

    executable_for_bundle_macos(&bundle_id)
        .with_context(|| format!("found bundle {} but could not locate its executable", bundle_id))
}

/// Read ~/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist
/// and extract the bundle ID registered as the default https:// handler.
///
/// Uses `plutil -convert json` to get reliable JSON rather than parsing the
/// human-readable plist text format, which has nested dicts that trip up
/// simple line-by-line parsers.
#[cfg(target_os = "macos")]
fn default_browser_bundle_id_macos() -> Result<String> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let plist = format!(
        "{}/Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist",
        home
    );

    // plutil converts the binary/XML plist to JSON and writes it to stdout (-o -)
    let output = Command::new("plutil")
        .args(["-convert", "json", "-o", "-", &plist])
        .output()
        .context("failed to run plutil — is macOS developer tools installed?")?;

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("plutil output was not valid JSON")?;

    // LSHandlers is an array of dicts; each dict has LSHandlerURLScheme and LSHandlerRoleAll.
    // Some dicts also have a nested LSHandlerPreferredVersions dict that contains its own
    // LSHandlerRoleAll = "-" — we want the top-level one, which serde_json gives us directly.
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

/// Scan /Applications for a .app whose CFBundleIdentifier matches `bundle_id`
/// case-insensitively, then return the path to its executable binary.
///
/// We avoid mdfind here because LaunchServices and Spotlight index bundle IDs
/// with inconsistent casing (e.g. LaunchServices stores "com.brave.browser"
/// but the app's Info.plist and Spotlight index have "com.brave.Browser").
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

        // Read CFBundleIdentifier from the app's Info.plist
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

        // Found the app — now get the binary name
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

/// Linux: ask xdg-settings for the default browser desktop entry,
/// then resolve the executable via the PATH.
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

    // Desktop entry filename → likely binary name: e.g. "brave-browser.desktop" → "brave-browser"
    let bin_name = desktop_entry.trim_end_matches(".desktop");

    which::which(bin_name)
        .with_context(|| format!("default browser '{}' not found on PATH", bin_name))
}

/// Launch the default Chromium-based browser, navigate to `url`, and return the full page HTML.
pub async fn fetch_html(url: &str) -> Result<String> {
    let binary = find_chromium_binary()
        .context("could not find a Chromium-based browser to launch")?;

    let config = BrowserConfig::builder()
        .chrome_executable(binary)
        // Suppress first-run dialogs and default-browser prompts that block CDP
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-default-apps")
        // Visible window for development. Switch to HeadlessMode::True for benchmarking.
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
    page.wait_for_navigation().await?;
    let html = page.content().await?;

    Ok(html)
}
