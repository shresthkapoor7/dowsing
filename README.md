# dowsing

> **Experimental.** Work in progress. Made by Shresth Kapoor.

Navigate any website to answer a question using embedding similarity. No LLM per navigation step. No API costs during traversal.

## The idea

Most browser agents call an LLM at every navigation decision. Every click costs money and time.

This tool embeds your question once. Every navigation decision is cosine similarity between the query embedding and link context embeddings. The LLM is never involved in navigation.

```
existing:   question -> LLM -> click -> LLM -> click -> LLM -> click -> answer
this tool:  question -> embed once -> similarity -> click -> similarity -> click -> plain text -> clipboard
```

**Output:** the plain text content of the most relevant page found, copied to your clipboard. Paste it into whatever LLM you want. No vendor lock-in, no API key required for retrieval.

Works on authenticated pages via your real Chrome profile. No credential handling needed.

## Status

This is an experiment in progress. The core navigation loop is being built out. Expect rough edges, missing features, and breaking changes.

Current focus: getting the end-to-end navigation loop working before optimizing anything.

## Usage

```bash
# download the embedding model (one-time)
cargo run --bin download-model

# navigate
cargo run -- "how to configure auth in NextAuth v5" "https://authjs.dev"

# with debug logs (writes raw HTML + extracted text to debug_logs/)
cargo run -- "what is ownership in rust" "https://doc.rust-lang.org/book/" --debug
```

## Examples

### Documentation sites

```bash
# Find how to configure middleware in Next.js
cargo run -- "how to write middleware in Next.js app router" "https://nextjs.org/docs"

# Look up a specific Rust std trait
cargo run -- "how does the Iterator flat_map method work" "https://doc.rust-lang.org/std/"

# Find Stripe's webhook signature verification docs
cargo run -- "how to verify webhook signatures" "https://docs.stripe.com"
```

### PDFs

Dowsing works on any page your browser can load — including PDFs rendered inline by Chrome.

```bash
# Navigate to a hosted research paper and extract the methodology section
cargo run -- "what dataset was used for evaluation" "https://arxiv.org/pdf/2401.00001"

# Pull terms from a hosted legal document
cargo run -- "termination clause and notice period" "https://example.com/contract.pdf"
```

### LinkedIn

Because dowsing connects to your running browser, it has access to your LinkedIn session. No login, no scraping — just navigate as if you were browsing yourself.

```bash
# Read someone's experience and skills from their profile
cargo run -- "current role and past experience" "https://www.linkedin.com/in/someprofile"

# Find job postings matching a role
cargo run -- "senior rust engineer remote" "https://www.linkedin.com/jobs/"
```

### Internal / authenticated pages

Any site your browser is already logged into works the same way — Notion, Confluence, GitHub, internal dashboards.

```bash
# Find a specific page in a Notion workspace
cargo run -- "Q2 roadmap milestones" "https://www.notion.so/yourworkspace"

# Look up a Confluence page
cargo run -- "deployment runbook for payments service" "https://yourcompany.atlassian.net/wiki"
```

## How it works

### Overview

1. Embed your query once using `all-MiniLM-L6-v2` (23MB ONNX model, runs locally)
2. Connect to your running browser via Chrome DevTools Protocol
3. Navigate autonomously using embedding similarity to pick the best links
4. Copy all relevant pages to clipboard, sorted by relevance

### Browser connection

Dowsing connects to your **running browser** — it doesn't launch a new instance or copy your profile. It opens tabs in your existing session, so authenticated pages just work. When it's done, it closes only the tabs it opened and disconnects, leaving your browser untouched.

If your browser doesn't have remote debugging enabled, dowsing restarts it with `--remote-debugging-port=9222`. Chromium-based browsers restore all tabs on restart.

### Content extraction

Raw HTML is full of nav bars, footers, and cookie banners. Before embedding a page, dowsing extracts the main content:

- Prefers `<article>` or `<main>` as the content root, falls back to `<body>`
- Strips `<script>`, `<style>`, `<nav>`, `<header>`, `<footer>`, `<aside>` subtrees
- Collapses whitespace into clean prose text

### Link context

Anchor text alone is useless to an embedder ("Click here" means nothing). For each link, dowsing builds a rich context string:

```
heading: NextAuth v5 Migration | text: configuration | context: see the configuration guide for auth options | url: https://...
```

This context string is what gets embedded and compared to the query. Links from content areas (`<main>`, `<article>`) are prioritized over sidebar/nav links, and duplicates are removed.

### Navigation loop

Each hop:

1. Fetch the current page, extract content, embed it, score against the query
2. Extract all links with context strings
3. Filter to same-domain links (prevents drifting to external sites)
4. Score each link's context string against the query embedding
5. Apply nav-bar decay: links appearing on >50% of visited pages get their score multiplied by 0.3
6. Fetch the top 3 candidates **in parallel** (beam search)
7. Score each candidate page, pick the best to continue from

The loop stops when:
- A page scores above the threshold (0.55)
- No candidate beats the current best score (peak detection)
- Max depth (10 hops) is reached
- No unvisited links remain

Dead-end detection: if scores drop for 2 consecutive hops, dowsing skips the top-ranked link and tries alternatives instead.

### Output

All visited pages are collected, deduplicated by URL, sorted by score, and copied to clipboard with metadata. Token count (GPT-4o tokenizer) is shown so you know if it fits your LLM's context window.

## Tech

- `chromiumoxide` — Chrome DevTools Protocol, connects to running browser
- `ort` — ONNX Runtime, runs all-MiniLM-L6-v2 locally
- `tokenizers` — HuggingFace tokenizer for the embedding model
- `tiktoken-rs` — GPT-4o tokenizer for output token counting
- Flat dot-product search (no HNSW, no vector DB — just math)

## Benchmarking

Will be evaluated against Exa's WebCode RAG dataset (317 query-answer pairs from real documentation). The hypothesis is that similarity-based navigation matches or approaches LLM-based navigation accuracy at a fraction of the latency and zero cost per query.

| | Groundedness | Avg Hops | Avg Time | Cost/query |
|---|---|---|---|---|
| dowsing | TBD | TBD | TBD | $0 |
| browser-use (GPT-4o) | TBD | TBD | TBD | ~$0.04 |