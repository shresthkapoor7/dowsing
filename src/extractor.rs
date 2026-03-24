// extractor.rs — DOM content extraction and link context building
//
// Phase 2 responsibility:
//   1. extract_page_content(html) → clean prose text (Readability-style)
//   2. extract_links(html, base_url) → Vec<LinkContext> with rich context strings
//
// Content extraction:
//   Prefers <article> or <main> as the content root, falls back to <body>.
//   Skips <script>, <style>, <nav>, <header>, <footer>, <aside> subtrees.
//
// Link context format (what gets embedded):
//   "heading: {nearest h1-h6} | text: {anchor text} | context: {parent block text} | url: {absolute url}"
//
// Why rich context strings: "Click here" is useless to an embedder.
// "heading: NextAuth v5 Migration | text: configuration | context: see the
//  configuration guide for auth options" is meaningful.

use anyhow::Result;
use scraper::{ElementRef, Html, Selector};

pub struct LinkContext {
    /// The formatted string that gets embedded for similarity scoring.
    pub context_string: String,
    /// The absolute URL this link points to.
    pub url: String,
}

// ---------------------------------------------------------------------------
// Content extraction
// ---------------------------------------------------------------------------

/// Extract the main readable content from an HTML page as plain text.
///
/// Uses a Readability-style heuristic: prefer <article> or <main>, fall back
/// to <body>. Strips script, style, nav, header, footer, and aside subtrees.
pub fn extract_page_content(html: &str) -> String {
    let document = Html::parse_document(html);

    let root = find_content_root(&document);
    let mut text = String::new();
    if let Some(node) = root {
        collect_text(node, &mut text);
    }

    // Collapse runs of whitespace
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Find the best content root element: <article>, <main>, [role="main"], or <body>.
fn find_content_root(document: &Html) -> Option<ElementRef<'_>> {
    for selector_str in &[
        "article",
        "main",
        r#"[role="main"]"#,
        "#content",
        ".content",
        "body",
    ] {
        if let Ok(sel) = Selector::parse(selector_str) {
            if let Some(el) = document.select(&sel).next() {
                return Some(el);
            }
        }
    }
    None
}

/// Recursively collect text from an element, skipping non-content subtrees.
fn collect_text(node: ElementRef, out: &mut String) {
    let tag = node.value().name();

    // Skip entire subtrees that aren't content
    if matches!(
        tag,
        "script" | "style" | "nav" | "header" | "footer" | "aside" | "noscript"
    ) {
        return;
    }

    for child in node.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
        } else if let Some(el) = ElementRef::wrap(child) {
            collect_text(el, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Link extraction
// ---------------------------------------------------------------------------

/// Extract all links from a page with rich context strings for embedding.
///
/// Walks the full document (including nav/header — Phase 3's nav-bar decay
/// heuristic handles suppressing those). Tracks the nearest preceding heading
/// and the parent block element text as context for each link.
///
/// Skips: fragment-only hrefs (#...), javascript: links, mailto:, empty anchor text.
pub fn extract_links(html: &str, base_url: &str) -> Vec<LinkContext> {
    let document = Html::parse_document(html);
    let mut links = Vec::new();
    let mut current_heading = String::new();

    // Walk from body so we capture links everywhere (nav bar decay handles noise in Phase 3)
    let body_sel = Selector::parse("body").unwrap();
    if let Some(body) = document.select(&body_sel).next() {
        walk_links(body, &mut current_heading, &mut links, base_url);
    }

    links
}

/// Recursive tree walk that tracks the current heading and emits LinkContext for each <a>.
fn walk_links(
    node: ElementRef,
    current_heading: &mut String,
    links: &mut Vec<LinkContext>,
    base_url: &str,
) {
    let tag = node.value().name();

    // Don't descend into script/style — no links we care about there
    if matches!(tag, "script" | "style" | "noscript") {
        return;
    }

    // Update current heading when we pass one
    if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
        let heading: String = node
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !heading.is_empty() {
            *current_heading = heading;
        }
    }

    // Emit a LinkContext when we hit an <a href="...">
    if tag == "a" {
        if let Some(href) = node.value().attr("href") {
            let text: String = node
                .text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

            // Skip empty text, fragment-only, and non-http schemes
            let skip = text.is_empty()
                || href.starts_with('#')
                || href.starts_with("javascript:")
                || href.starts_with("mailto:")
                || href.starts_with("tel:");

            if !skip {
                if let Ok(url) = resolve_url(base_url, href) {
                    let context = parent_block_text(node);
                    let heading_str = if current_heading.is_empty() {
                        "none"
                    } else {
                        current_heading.as_str()
                    };

                    let context_string = format!(
                        "heading: {} | text: {} | context: {} | url: {}",
                        heading_str, text, context, url
                    );

                    links.push(LinkContext {
                        context_string,
                        url,
                    });
                }
            }
        }
    }

    // Recurse into children
    for child in node.children() {
        if let Some(el) = ElementRef::wrap(child) {
            walk_links(el, current_heading, links, base_url);
        }
    }
}

/// Get the text of the nearest block-level ancestor as link context.
///
/// Walks up from the link element looking for a <p>, <li>, <td>, or <div>.
/// Caps at 200 chars to keep context strings a reasonable embedding size.
fn parent_block_text(link: ElementRef) -> String {
    // Walk up through parent elements
    let mut node = link.parent();
    while let Some(parent_ref) = node {
        if let Some(parent_el) = ElementRef::wrap(parent_ref) {
            let tag = parent_el.value().name();
            if matches!(tag, "p" | "li" | "td" | "th" | "div" | "section" | "article") {
                let text: String = parent_el
                    .text()
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if !text.is_empty() {
                    // Cap length so context strings stay reasonable
                    return text.chars().take(200).collect();
                }
            }
            node = parent_el.parent();
        } else {
            break;
        }
    }
    String::new()
}

/// Resolve a potentially-relative href to an absolute URL.
fn resolve_url(base: &str, href: &str) -> Result<String> {
    // Already absolute
    if href.starts_with("http://") || href.starts_with("https://") {
        return Ok(href.to_string());
    }

    // Protocol-relative or relative — let url::Url handle it
    let base_url = url::Url::parse(base)
        .map_err(|e| anyhow::anyhow!("invalid base URL '{}': {}", base, e))?;

    let resolved = base_url
        .join(href)
        .map_err(|e| anyhow::anyhow!("failed to resolve '{}' against '{}': {}", href, base, e))?;

    // Only keep http/https
    if resolved.scheme() == "http" || resolved.scheme() == "https" {
        Ok(resolved.to_string())
    } else {
        anyhow::bail!("non-http scheme after resolution: {}", resolved)
    }
}
