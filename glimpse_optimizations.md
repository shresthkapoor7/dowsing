# Optimizations from Glimpse

Techniques from the glimpse codebase (`/Users/shresthkapoor/code/glimpse`) that can make dowsing faster.

## High impact

### 1. Batch embedding with Rayon

Glimpse uses `rayon::par_bridge()` and `par_iter()` for CPU-bound work, processing in chunks of 256. Currently dowsing embeds each link context string one at a time sequentially in a loop (`embedder.embed(&link.context_string)` in navigator.rs line 153). A page can yield dozens of links — embedding them all serially is the main bottleneck.

**Options:**
- Batch all context strings into a single ONNX inference call (pad/stack into one tensor, run once, split output)
- Or parallelize individual embeds across CPU cores with rayon

Batched ONNX is faster than rayon parallelism here since it avoids per-call session overhead.

### 2. Embedding cache (deduplication)

Glimpse's LSP resolver groups identical `(callee, qualifier, ext)` tuples and resolves once, then applies the result everywhere. Dowsing re-embeds the same link context strings across hops — nav links like "Home", "Docs", "API Reference" appear on every page with identical context and get embedded every time.

**Approach:**
- `HashMap<u64, Vec<f32>>` keyed by hash of the input text
- Before embedding, check cache. On hit, skip ONNX entirely
- This compounds with batch embedding — cache hits reduce batch size

### 3. HTML-to-Markdown for extraction

Glimpse has a lightweight HTML-to-Markdown converter (`fetch/url.rs`) that walks the DOM tree with tag-specific handlers (headings, lists, links, code blocks). This could be faster than Readability-based extraction for stripping DOM noise, and the markdown output embeds better than raw prose since structure is preserved.

**Worth benchmarking** against the current `extractor::extract_page_content` to see if it improves both speed and embedding quality.

## Medium impact

### 4. Lazy initialization with OnceLock

Glimpse uses `OnceLock::new()` for expensive one-time setup. Check if dowsing's ONNX session and tokenizer initialization can benefit — if they're already loaded once and reused, no change needed. If there's any per-call overhead, `OnceLock` eliminates it.

### 5. Binary serialization with bincode

Glimpse persists its index to disk using bincode for fast serialization. If dowsing adds cross-session embedding caching (e.g. caching page embeddings for previously visited URLs), bincode is the right format — much faster than JSON for `Vec<f32>` data.

## Implementation priority

1. Batch embedding (biggest single speedup)
2. Embedding cache (compounds with batching, easy to add)
3. HTML-to-Markdown extraction (needs benchmarking first)
4. Lazy init audit (quick check, small gain)
5. Bincode cache persistence (only if cross-session caching is needed)
