# Semantic Web Navigator

Navigate any website to answer a question using embedding similarity. No LLM per navigation step. No API costs during traversal. Works on authenticated pages via the user's real Chrome profile.

## Core idea

Existing browser agents call an LLM at every navigation decision. Every click costs money and time.

This tool embeds the question once. Every navigation decision is cosine similarity between the query embedding and link context embeddings. The LLM is never involved in navigation. Only called at the end if you want answer synthesis.

```
existing:   question → LLM → click → LLM → click → LLM → click → answer
this tool:  question → embed once → similarity → click → similarity → click → answer
```

## Repository structure

```
semantic-navigator/
├── src/
│   ├── main.rs              # CLI entry point
│   ├── browser.rs           # Playwright via headless_chrome or chromiumoxide
│   ├── extractor.rs         # DOM extraction, link context building, Readability
│   ├── embedder.rs          # ONNX model loading, text embedding, mean pooling
│   ├── index.rs             # in-memory vector index
│   ├── search.rs            # cosine similarity, top-k, nav heuristics
│   ├── navigator.rs         # main navigation loop
│   └── eval.rs              # benchmarking harness
├── eval/
│   └── webcode_rag.jsonl    # Exa's 317 query-answer pairs
├── models/
│   ├── model.onnx           # all-MiniLM-L6-v2 ONNX export
│   └── tokenizer.json       # HuggingFace tokenizer config
├── Cargo.toml
└── README.md
```

## Browser control

Two options for driving Chrome from Rust:

- `chromiumoxide` — async, full Chrome DevTools Protocol, actively maintained, recommended
- `headless_chrome` — simpler API, less control

Use `chromiumoxide`. It launches Chrome with a persistent user data directory which gives access to the user's existing cookies and sessions. This is how authenticated pages work — no credential handling, the browser already has the session.

Chrome profile paths:

- macOS: `~/Library/Application Support/Google/Chrome/Default`
- Linux: `~/.config/google-chrome/Default`
- Windows: `%LOCALAPPDATA%\Google\Chrome\User Data\Default`

Run with `headless: false` initially for debugging. Switch to headless for benchmarking.

## Content extraction

Raw HTML is full of nav bars, footers, cookie banners. Before embedding a page or extracting links, clean the HTML first.

For page content: port Mozilla's Readability algorithm or use the `readability` crate. Extract only the main body — prose, code blocks, tables. Strip everything else.

For links: don't just grab anchor text. Build a context string per link:

```
"heading: {nearest heading} | text: {anchor text} | context: {surrounding paragraph text} | url: {absolute url}"
```

This string is what gets embedded. "Click here" is useless. "heading: NextAuth v5 Migration | text: configuration | context: see the configuration guide for auth options" is meaningful.

## Embedder

Model: `all-MiniLM-L6-v2` exported to ONNX format. 23MB. 384 dimensions. Fast inference. Good retrieval quality.

Rust crates:

- `ort` — ONNX Runtime bindings for Rust
- `tokenizers` — HuggingFace tokenizers

Pipeline per text input:

1. Tokenize with HuggingFace tokenizer
2. Run ONNX session — outputs token-level embeddings shape `[1, seq_len, 384]`
3. Mean pool over token dimension to get `[384]`
4. L2 normalize the result

Normalize so dot product equals cosine similarity. This avoids computing magnitudes at search time.

Embed the query once at startup. Cache it. Never re-embed the query.

## Vector index

Simple in-memory structure. For the scale of links on a documentation site (hundreds to low thousands per session) there is no need for HNSW or any approximate nearest neighbor structure. Flat linear scan is fast enough and exact.

Each entry: `(text: String, embedding: Vec<f32>)`

At search time: compute dot product between query embedding and every entry embedding, return top-k by score.

This is the right call for this problem. Do not over-engineer the index.

## Navigation algorithm

```
navigate(query, start_url, max_depth=10, threshold=0.75):

  query_embedding = embed(query)
  visited = empty set
  score_history = empty vec
  best_result = None
  link_frequency = empty map
  current_url = start_url

  for hop in 0..max_depth:
    load current_url in browser
    wait for page idle

    page_content = extract_page_content()
    page_score = dot(query_embedding, embed(page_content))

    if page_score > best_result.score:
      best_result = (current_url, page_content, hop, page_score)

    if page_score >= threshold:
      break

    visited.add(current_url)
    links = extract_links()
    links = filter out visited URLs

    if links is empty:
      break

    update link_frequency counts

    for each link:
      score = dot(query_embedding, embed(link.context_string))
      if link_frequency[url] / hop_count > 0.5:
        score *= 0.3   # nav bar decay

    sort links by score descending

    score_history.push(page_score)
    if last 2 scores are decreasing:
      follow second-best link   # dead end backtrack
    else:
      follow best link

  return best_result
```

## Navigation heuristics

**Cycle avoidance:** Hard exclude visited URLs. Without this the navigator loops on nav bars forever.

**Dead end detection:** If page content score decreases for 2 consecutive hops, follow the second-highest scored link instead of the best. The greedy best was a dead end.

**Nav bar decay:** Track how often each URL appears across extracted link sets. Links appearing on more than 50% of visited pages get their score multiplied by 0.3. Suppresses persistent elements like home, about, pricing that appear everywhere.

**Stopping condition:** Page content score above 0.75 means we have found a highly relevant page. Stop. Return it.

**Max depth:** Hard cap at 10 hops. Prevents runaway navigation on sites with poor structure.

## Benchmarking

### Dataset

Exa open-sourced their WebCode benchmark alongside their March 2026 paper. The RAG dataset contains 317 `{query, expected_answer, start_url}` triples sourced from real documentation — GNU, W3C, IETF RFCs, Python, Rust, Go official docs.

These queries were specifically chosen because frontier models fail to answer them from memory alone. This means correct navigation actually matters — parametric knowledge cannot substitute for finding the right page.

Download from: `https://github.com/exa-labs/benchmarks`

### Metrics

**Groundedness:** Does the page the navigator returned actually contain the correct answer. Scored by an LLM judge given the page content and expected answer. Binary per query, averaged across the dataset. This is the same metric Exa uses in their WebCode paper — using their own metric against their own dataset is intentional.

**Hop count:** Number of pages visited before stopping. Lower is better. Measures navigation efficiency.

**Time per query:** Wall clock seconds from query input to result returned.

**Cost per query:** Always $0 for this tool. Compared against LLM-based baselines which have real costs.

### Baseline comparison

Run the same 317 queries through at least one LLM-based browser agent — browser-use is the simplest to set up. Record groundedness, hop count, time, and estimated cost per query.

Present results as a table:


|                      | Groundedness | Avg Hops | Avg Time | Cost/query |
| -------------------- | ------------ | -------- | -------- | ---------- |
| This tool            | X%           | X        | Xs       | $0         |
| browser-use (GPT-4o) | X%           | X        | Xs       | ~$0.04     |


### What to look for

The hypothesis is that semantic similarity navigation matches or approaches LLM-based navigation accuracy on structured documentation sites, at a fraction of the latency and zero cost.

If groundedness is within 10-15% of the LLM baseline, that is a strong result given the cost and speed difference. If it exceeds the LLM baseline on any subset — especially fresh post-training-cutoff documentation — that is a publishable finding.

## Cargo.toml dependencies

```
chromiumoxide      — Chrome DevTools Protocol, browser control
ort                — ONNX Runtime bindings
tokenizers         — HuggingFace tokenizers
ndarray            — array math for mean pooling
tokio              — async runtime
serde / serde_json — serialization
anyhow             — error handling
clap               — CLI argument parsing
reqwest            — HTTP for downloading eval dataset
```

## CLI interface

```
semantic-navigator --query "how to configure auth in NextAuth v5" --start "https://authjs.dev"
semantic-navigator --query "breaking changes in prisma v6" --start "https://www.prisma.io/docs"
semantic-navigator eval --dataset eval/webcode_rag.jsonl --output results.json
semantic-navigator eval --dataset eval/webcode_rag.jsonl --compare browser-use
```

## Build and run

```bash
# download model files
cargo run --bin download-model

# run navigator
cargo run -- --query "..." --start "https://..."

# run benchmark
cargo run -- eval --dataset eval/webcode_rag.jsonl
```

## Implementation phases

### Phase 1: Primitives

Get each core primitive working in isolation before wiring anything together.

1. Get `chromiumoxide` launching Chrome, loading a page, and returning raw HTML
2. Get `ort` loading the ONNX model and producing a `[384]` embedding for a hardcoded string

Done when: you can print an embedding vector from a string and a page URL independently.

### Phase 2: Integration

Wire the two primitives together into the minimal end-to-end path.

3. Load a page in the browser, extract its text content, embed it, compute cosine similarity against a query embedding, print the score
4. Build link extraction with context strings — the `"heading: ... | text: ... | context: ... | url: ..."` format

Done when: you can run `cargo run -- --query "..." --start "https://..."` and see a page score plus a list of scored links printed to stdout.

### Phase 3: Navigation loop

Build the core navigation logic.

5. Navigation loop without heuristics — just greedy best-scored link each hop, cycle avoidance (visited set), stopping condition, max depth
6. Add heuristics one at a time: dead end detection (score history backtrack), then nav bar decay (link frequency map)

Done when: the navigator follows links autonomously and returns the best page found.

### Phase 4: Eval and benchmarking

7. Add the eval harness — load `webcode_rag.jsonl`, run each query, record groundedness (LLM judge), hop count, time
8. Benchmark against browser-use baseline and produce the comparison table

