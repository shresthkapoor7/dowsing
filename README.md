# dowsing

> **Experimental.** Work in progress. Things will break.

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
# run navigator
cargo run -- --query "how to configure auth in NextAuth v5" --start "https://authjs.dev"

# run benchmark
cargo run -- eval --dataset eval/webcode_rag.jsonl
```

## How it works

1. Embeds your query using `all-MiniLM-L6-v2` (23MB ONNX model, runs locally)
2. Loads the start URL in Chrome using your existing browser session
3. Extracts links with context strings (heading + anchor text + surrounding paragraph)
4. Scores each link by cosine similarity to the query embedding
5. Follows the best link, repeats until a high-relevance page is found or max depth is hit
6. Copies the page content to clipboard

## Tech

- `chromiumoxide` for Chrome DevTools Protocol browser control
- `ort` for local ONNX inference
- `tokenizers` for HuggingFace tokenizer
- Flat cosine similarity search (no HNSW, no vector DB, just math)

## Benchmarking

Will be evaluated against Exa's WebCode RAG dataset (317 query-answer pairs from real documentation). The hypothesis is that similarity-based navigation matches or approaches LLM-based navigation accuracy at a fraction of the latency and zero cost per query.

| | Groundedness | Avg Hops | Avg Time | Cost/query |
|---|---|---|---|---|
| dowsing | TBD | TBD | TBD | $0 |
| browser-use (GPT-4o) | TBD | TBD | TBD | ~$0.04 |