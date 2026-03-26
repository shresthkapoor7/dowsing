// navigator.rs — autonomous navigation loop
//
// Follows links across hops using embedding similarity, returns ALL relevant
// pages found (not just one). Key heuristics:
//
//   - Hard same-domain filter: stay on the start URL's site
//   - Minimum content length: skip stub/index pages (<50 words) that get
//     artificially high embedding scores due to short text + keyword overlap
//   - Peak detection: stop when all parallel candidates score lower than the
//     current best — we've found the most relevant area
//   - Dead-end detection: 2 consecutive score drops → try alternative links
//   - Nav-bar decay: suppress links that appear on >50% of visited pages
//   - Parallel beam: fetch top-3 links per hop via join_all, cache best HTML

use crate::browser;
use crate::embedder::Embedder;
use crate::extractor;
use anyhow::Result;
use chromiumoxide::Browser;
use std::collections::{HashMap, HashSet};

const MAX_DEPTH: usize = 10;
const THRESHOLD: f32 = 0.55;
const BEAM: usize = 3;
const MIN_WORDS: usize = 50; // pages with fewer words are stubs/indexes — skip scoring

#[derive(Clone)]
pub struct PageResult {
    pub url: String,
    pub content: String,
    pub score: f32,
    pub hop: usize,
}

pub struct NavResult {
    /// All pages found, sorted by score descending
    pub pages: Vec<PageResult>,
}

pub async fn navigate(
    query_embedding: &[f32],
    start_url: &str,
    browser: &Browser,
    opened_pages: &browser::OpenedPageTracker,
    embedder: &mut Embedder,
) -> Result<NavResult> {
    let start_domain = domain(start_url);

    let mut visited: HashSet<String> = HashSet::new();
    let mut score_history: Vec<f32> = Vec::new();
    let mut all_pages: Vec<PageResult> = Vec::new();
    let mut link_frequency: HashMap<String, usize> = HashMap::new();
    let mut current_url = start_url.to_string();
    let mut cached_html: Option<(String, String)> = None;
    let mut best_score: f32 = f32::NEG_INFINITY;

    for hop in 0..MAX_DEPTH {
        let html = if cached_html
            .as_ref()
            .map_or(false, |(u, _)| u == &current_url)
        {
            println!("[hop {}] {} (cached)", hop, current_url);
            cached_html.take().unwrap().1
        } else {
            println!("[hop {}] fetching {}", hop, current_url);
            browser::fetch_page(browser, &current_url, opened_pages).await?
        };

        let page_content = extractor::extract_page_content(&html);
        let word_count = page_content.split_whitespace().count();

        if page_content.is_empty() || word_count < MIN_WORDS {
            println!(
                "[hop {}] stub page ({} words) — skipping score",
                hop, word_count
            );
            visited.insert(current_url.clone());
            // Don't break — try to extract links and continue
        } else {
            let page_embedding = embedder.embed(&page_content)?;
            let page_score = dot(query_embedding, &page_embedding);
            println!("[hop {}] score: {:.4}  ({} words)  {}", hop, page_score, word_count, current_url);

            // Collect every real page we visit
            all_pages.push(PageResult {
                url: current_url.clone(),
                content: page_content,
                score: page_score,
                hop,
            });

            if page_score > best_score {
                best_score = page_score;
            }

            if page_score >= THRESHOLD {
                println!("[nav] score {:.4} >= threshold {:.2} — stopping", page_score, THRESHOLD);
                break;
            }

            score_history.push(page_score);
        }

        visited.insert(current_url.clone());

        // --- Link extraction and scoring ---
        let links = extractor::extract_links(&html, &current_url);
        let links: Vec<_> = links
            .into_iter()
            .filter(|l| !visited.contains(&l.url))
            .collect();

        if links.is_empty() {
            println!("[nav] no unvisited links — stopping");
            break;
        }

        // Hard same-domain filter
        let same_domain: Vec<_> = links
            .iter()
            .filter(|l| domain(&l.url) == start_domain)
            .collect();

        let scoring_pool: Vec<&extractor::LinkContext> = if same_domain.is_empty() {
            println!("[nav] no same-domain links — allowing cross-domain");
            links.iter().collect()
        } else {
            same_domain
        };

        for link in &scoring_pool {
            *link_frequency.entry(link.url.clone()).or_insert(0) += 1;
        }

        let hop_count = (hop + 1) as f32;
        let mut scored_links: Vec<(f32, &str)> = scoring_pool
            .iter()
            .filter_map(|link| {
                let emb = embedder.embed(&link.context_string).ok()?;
                let mut score = dot(query_embedding, &emb);

                let freq = *link_frequency.get(&link.url).unwrap_or(&0) as f32;
                if freq / hop_count > 0.5 {
                    score *= 0.3;
                }

                Some((score, link.url.as_str()))
            })
            .collect();

        scored_links.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        if scored_links.is_empty() {
            println!("[nav] no scorable links — stopping");
            break;
        }

        // Dead-end: 2 consecutive drops (need at least 3 data points)
        let is_dead_end = score_history.len() >= 3
            && score_history[score_history.len() - 1] < score_history[score_history.len() - 2]
            && score_history[score_history.len() - 2] < score_history[score_history.len() - 3];

        let candidates: Vec<&str> = if is_dead_end && scored_links.len() > 1 {
            println!("[nav] 2 consecutive drops — trying alternative links");
            scored_links[1..].iter().take(BEAM).map(|(_, u)| *u).collect()
        } else {
            scored_links.iter().take(BEAM).map(|(_, u)| *u).collect()
        };

        println!("[hop {}] fetching {} candidate(s):", hop, candidates.len());
        for c in &candidates {
            println!("        {}", c);
        }

        let fetch_results: Vec<(String, Result<String>)> = futures::future::join_all(
            candidates.iter().map(|url| {
                let url = url.to_string();
                let opened_pages = opened_pages.clone();
                async move {
                    let html = browser::fetch_page(browser, &url, &opened_pages).await;
                    (url, html)
                }
            }),
        )
        .await;

        // Score every fetched page, collect results, pick next hop
        let mut best_next_score = f32::NEG_INFINITY;
        let mut best_next_url = candidates[0].to_string();
        let mut best_next_html = String::new();

        for (url, result) in &fetch_results {
            visited.insert(url.clone());
            if let Ok(page_html) = result {
                let content = extractor::extract_page_content(page_html);
                let wc = content.split_whitespace().count();
                if wc < MIN_WORDS {
                    println!("        skip ({} words)  {}", wc, url);
                    continue;
                }
                if let Ok(emb) = embedder.embed(&content) {
                    let s = dot(query_embedding, &emb);
                    println!("        {:.4}  ({} words)  {}", s, wc, url);

                    all_pages.push(PageResult {
                        url: url.clone(),
                        content,
                        score: s,
                        hop: hop + 1,
                    });

                    if s > best_next_score {
                        best_next_score = s;
                        best_next_url = url.clone();
                        best_next_html = page_html.clone();
                    }
                }
            }
        }

        // Stop if a parallel candidate cleared the threshold
        if best_next_score >= THRESHOLD {
            println!(
                "[nav] candidate scored {:.4} >= {:.2} — stopping",
                best_next_score, THRESHOLD
            );
            break;
        }

        // Peak detection: if none of the candidates beat the current best,
        // we've reached the most relevant area. Stop exploring.
        if best_next_score < best_score {
            println!(
                "[nav] peaked — no candidate ({:.4}) beats best ({:.4}), stopping",
                best_next_score, best_score
            );
            break;
        }

        if best_next_score > best_score {
            best_score = best_next_score;
        }

        current_url = best_next_url;
        if !best_next_html.is_empty() {
            cached_html = Some((current_url.clone(), best_next_html));
        }
    }

    // Deduplicate by URL (keep the highest score for each), then sort descending
    all_pages.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut seen_urls: HashSet<String> = HashSet::new();
    all_pages.retain(|p| seen_urls.insert(p.url.clone()));

    if all_pages.is_empty() {
        anyhow::bail!("navigation produced no results");
    }

    Ok(NavResult { pages: all_pages })
}

fn domain(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}
