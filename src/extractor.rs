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
// Link context format:
//   "heading: {nearest h1-h6} | text: {anchor text} | context: {parent block text} | url: {absolute url}"

use anyhow::Result;
use scraper::{ElementRef, Html, Selector};

#[derive(Debug, Clone)]
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
pub fn extract_page_content(html: &str) -> String {
    let document = Html::parse_document(html);

    let root = find_content_root(&document);
    let mut text = String::new();
    if let Some(node) = root {
        collect_text(node, &mut text);
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

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

fn collect_text(node: ElementRef, out: &mut String) {
    let tag = node.value().name();

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
/// Walks content areas first (`<main>`, `<article>`) where headings are
/// meaningful, then the full body for sidebar/nav links we missed.
/// Deduplicates by URL, keeping the link with the longest context string.
pub fn extract_links(html: &str, base_url: &str) -> Vec<LinkContext> {
    let document = Html::parse_document(html);
    let mut links = Vec::new();
    let mut current_heading = String::new();

    // Walk content areas first — these have real, relevant headings
    for sel_str in &["main", "article", r#"[role="main"]"#] {
        if let Ok(sel) = Selector::parse(sel_str) {
            if let Some(el) = document.select(&sel).next() {
                walk_links(el, &mut current_heading, &mut links, base_url);
            }
        }
    }

    // Then walk the full body to catch sidebar/nav links we missed
    let body_sel = Selector::parse("body").unwrap();
    if let Some(body) = document.select(&body_sel).next() {
        let content_urls: std::collections::HashSet<String> =
            links.iter().map(|l| l.url.clone()).collect();
        let mut nav_links = Vec::new();
        let mut nav_heading = String::new();
        walk_links(body, &mut nav_heading, &mut nav_links, base_url);
        for link in nav_links {
            if !content_urls.contains(&link.url) {
                links.push(link);
            }
        }
    }

    // Deduplicate by URL — keep the entry with the longest context string
    dedup_links(&mut links);

    links
}

fn dedup_links(links: &mut Vec<LinkContext>) {
    let mut best: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut to_remove = Vec::new();

    for (i, link) in links.iter().enumerate() {
        if let Some(&prev_idx) = best.get(&link.url) {
            if link.context_string.len() > links[prev_idx].context_string.len() {
                to_remove.push(prev_idx);
                best.insert(link.url.clone(), i);
            } else {
                to_remove.push(i);
            }
        } else {
            best.insert(link.url.clone(), i);
        }
    }

    to_remove.sort_unstable();
    for idx in to_remove.into_iter().rev() {
        links.remove(idx);
    }
}

fn walk_links(
    node: ElementRef,
    current_heading: &mut String,
    links: &mut Vec<LinkContext>,
    base_url: &str,
) {
    let tag = node.value().name();

    if matches!(tag, "script" | "style" | "noscript") {
        return;
    }

    // Reset heading when entering a content area — prevents headings from
    // popups/help divs bleeding into content link contexts
    if matches!(tag, "main" | "article") {
        *current_heading = String::new();
    }

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

    if tag == "a" {
        if let Some(href) = node.value().attr("href") {
            let text: String = node
                .text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");

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

    for child in node.children() {
        if let Some(el) = ElementRef::wrap(child) {
            walk_links(el, current_heading, links, base_url);
        }
    }
}

fn parent_block_text(link: ElementRef) -> String {
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

fn resolve_url(base: &str, href: &str) -> Result<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return Ok(href.to_string());
    }

    let base_url = url::Url::parse(base)
        .map_err(|e| anyhow::anyhow!("invalid base URL '{}': {}", base, e))?;

    let resolved = base_url
        .join(href)
        .map_err(|e| anyhow::anyhow!("failed to resolve '{}' against '{}': {}", href, base, e))?;

    if resolved.scheme() == "http" || resolved.scheme() == "https" {
        Ok(resolved.to_string())
    } else {
        anyhow::bail!("non-http scheme after resolution: {}", resolved)
    }
}
