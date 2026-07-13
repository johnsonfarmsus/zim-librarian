# ZIM Librarian — Technical Plan

A fully offline "AI Librarian": ask questions in plain language, get answers
grounded strictly in a local library of ZIM files, every claim backed by a
clickable citation that opens the exact source passage.

## Requirements recap (all hard)

1. Library format: Kiwix **ZIM** — drop-in compatibility with the existing ecosystem.
2. Open-weight models; **OLMo** preferred baseline, efficient alternatives selectable.
3. Runs on consumer hardware: Windows / macOS / Linux / phones / tablets, no GPU required.
4. Install ≈ "download one file, run it" — no Docker/Python/Node, no CLI.
5. Zero network access forever after installation.
6. Inline citations, each clickable, opening the exact ZIM passage.
7. Simple conversational chat UI.

## Architecture at a glance

```
┌────────────────────────── one process ──────────────────────────┐
│  Native shell (Tauri 2 webview)  or  browser (headless binary)  │
│        │ HTTP on 127.0.0.1:<random port> (never 0.0.0.0)        │
│  ┌─────▼─────────────────────────────────────────────────────┐  │
│  │ librarian-server (axum): UI assets · JSON API · SSE chat  │  │
│  │                          /content/<zim>/<path> (+highlight)│ │
│  └─────┬─────────────────────────────────────────────────────┘  │
│  ┌─────▼──────────────── librarian-core ─────────────────────┐  │
│  │ Library manager ──► zimlib (pure-Rust ZIM reader)          │ │
│  │ Indexer/Retriever ─► tantivy BM25 over ~1kB passages       │ │
│  │ Engine trait ──────► llama.cpp in-process (GGUF)           │ │
│  └────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

Everything is Rust in a single process. The web-tech UI is embedded in the
binary; the only IPC is localhost HTTP, which doubles as the ZIM article
renderer (citations are just links into it).

## Tooling decisions and reasoning

### App packaging: Tauri 2 (native shell) over Electron / pure-native / server-only

- **Why not Electron:** ships a whole Chromium (~200 MB baseline), heavy RAM
  idle, no mobile story. Violates "smallest footprint".
- **Why not fully native (SwiftUI/WinUI/GTK…):** 3–5 divergent UI codebases;
  maintenance cost is the opposite of "most maintainable".
- **Why Tauri 2:** uses the OS webview (WKWebView/WebView2/WebKitGTK), so
  installers are a few MB plus our binary; one Rust codebase; and it is the
  only mainstream option that also targets **iOS and Android**, which the
  requirements include. Produces normal installers (.dmg, .msi/.exe,
  .AppImage/.deb, .apk, .ipa) — download, open, done.
- **Escape hatch kept deliberately:** the core is a library plus a localhost
  server, and `crates/librarian` is a single headless binary that serves the
  same UI to the default browser. That gives a zero-toolchain dev loop, a
  fallback for platforms where webviews are troublesome, and makes the Tauri
  shell ~40 lines of glue with no app logic in it.

### ZIM access: our own pure-Rust reader (`zimlib`) over libzim bindings

- The official path (C++ `libzim` + Xapian) is the single hardest thing to
  cross-compile and bundle for 5 platforms. It would dominate build complexity.
- The ZIM format itself is small and stable (fixed 80-byte header, pointer
  lists, dirents, compressed clusters). A read-only implementation is ~450
  lines and is fully covered by the openzim `zim-testing-suite` files, plus
  real Kiwix ZIMs, in our test-suite. Supports ZIM v5/v6, both namespace
  schemes, uncompressed/XZ/Zstandard clusters — which covers current Kiwix
  output (zstd since 2020, xz before that).
- We intentionally do **not** use the Xapian full-text index embedded in many
  ZIMs: reading it would drag in Xapian (C++), it indexes whole articles (we
  need passages for precise citations), and not every ZIM has one.

### Retrieval: tantivy BM25 over passage chunks — **no embeddings by default**

The requirement is "smallest, fastest solution that reliably produces
accurate citations". That points at lexical retrieval, not vectors:

- **Passage-level BM25 gives exact citations for free.** The retrieved unit
  *is* the passage shown to the model and linked in the UI — nothing to
  re-align after generation.
- **No second model.** Embeddings would add a ~100–600 MB encoder, an
  embedding pass over every article at import (hours for full Wikipedia on a
  laptop, versus text-only BM25 indexing which is bounded by decompression
  speed), and a vector store. That's a large footprint increase for quality
  gains that mostly matter on paraphrase-heavy queries.
- **Encyclopedic content is keyword-friendly**, and we normalize questions
  (stopword stripping) before querying; the LLM tolerates modest retrieval
  noise because it sees several passages (default 6).
- tantivy is pure Rust, ~fast (100k+ docs/s indexing), memory-mapped at query
  time, Apache-2.0. ONE global index shared by all books — global corpus
  statistics make BM25 scores comparable across books, so a 373k-passage wiki
  can't drown out a small one on term-statistics alone. Removing a book is a
  delete-by-term on its ZIM UUID; indexing runs on a background thread with
  visible progress in the UI.
- **Source quality** (`merge_passages`): at most 2 passages per article and a
  relevance floor (drop anything under 35% of the top hit). No forced
  per-book representation — that experiment pulled in irrelevant passages
  just to have multiple books; diversity now has to be earned by score.
- **LLM-planned retrieval**: before searching, the model reads the
  conversation and either writes a self-contained keyword query (resolving
  pronouns and follow-up context) or declares that no new retrieval is
  needed (pure refinements reuse the previous sources). Falls back to a
  keyword heuristic when no model is loaded.
- **Source triage**: retrieved candidates are over-fetched (~10) and the
  model judges each passage's relevance in isolation *before* answering —
  small models are far better at "is this passage about X? yes/no" than at
  ignoring junk mid-answer. Survivors become the numbered sources; if
  nothing survives, the reply is a deterministic "not in your library"
  message listing the near-misses as clickable cards — no generation, no
  opportunity to hallucinate.
- **Honest failure**: answers that end up with zero supported citations are
  prefixed with an explicit "nothing in your library supported this" notice
  instead of masquerading as grounded.
- **Known ceiling (2B-class models)**: near-topic passages (a page *about*
  first-aid kits for a burn-treatment question) can survive triage, and the
  model may then blend its own knowledge with decorative citations. A
  larger judge/answer model (OLMo 7B+) tightens this; the deeper fix is
  hybrid retrieval with a small embedding model (the documented upgrade
  path).
- **Upgrade path:** `GlobalIndex` is the only retrieval seam; hybrid
  BM25+embedding reranking can be added later without touching anything else.

### Inference: llama.cpp **in-process** (`llama-cpp-2` bindings) over Ollama

- **Ollama** would mean a second daemon to install/start/babysit, its own
  model registry, and a background service violating "minimal background
  resource use". It's llama.cpp underneath anyway.
- Embedding llama.cpp gives: zero extra processes, GGUF files as plain
  model artifacts, Metal on macOS/iOS, CPU everywhere else, and quantized
  models that run on 8 GB laptops and recent phones.
- Generation is strictly serialized (one at a time) and the context is
  created per request, so idle RAM ≈ model weights only, and zero CPU.
- The engine sits behind a 2-method trait; a deterministic **extractive
  fallback** (quotes top passages, still cited/clickable) runs when no model
  is installed — the app is useful before any model download, and it's what
  the test-suite uses.

### Models: OLMo baseline, curated open alternatives

- Shipped default: **OLMo-2-1B-Instruct** (Q4_K_M GGUF, ~0.9 GB) — fully
  open (weights *and* data/training code), fine on 8 GB RAM — bundled into
  desktop installers and auto-selected on first run.
- Stronger hardware: OLMo-3-7B-Instruct (~4.5 GB Q4) or Gemma 4 E2B
  (~2.9 GB, best grounding quality tested; open-weight, not OSI-open).
  Hardware below that runs extractive mode, or any tiny .gguf added manually.
- Adding models: curated catalog with one-click downloads, a `.gguf` file
  picker (used in place), a paste-a-URL fetch box, or the drop folder —
  chat templates come from the GGUF metadata, so new model families work
  without code changes (with a manual Gemma/ChatML fallback for templates
  llama.cpp's engine can't parse).
- Distribution: desktop installers bundle the default model (~1 GB
  installers; strict "no network after install"); mobile and the headless
  binary offer it as a pre-checked download on the first-run setup screen,
  alongside the curated starter library of ZIMs (Wikipedia vital articles,
  WikiMed, OpenStreetMap wiki — resolved to the current edition via the
  Kiwix OPDS catalog at download time).

### Grounding & citations

1. Question → stopword-stripped BM25 query → top-k passages across all books
   (scores merged, deduped).
2. Prompt: system message with numbered sources + strict rules ("answer ONLY
   from sources, cite [n] after every claim, say so if sources are
   insufficient"), a short window of chat history, then the question.
3. The UI receives the source list *before* generation (SSE event), then
   streams tokens; `[n]` markers render as clickable chips.
4. Clicking a citation opens the original article served straight out of the
   ZIM at `/content/<zim>/<path>?hl=<passage prefix>`; an injected ~30-line
   script finds the passage text, wraps it in `<mark>`, and scrolls it into
   view. Article CSS/images resolve as relative links into the same route, so
   pages render as they do in Kiwix.

### Frontend: ~600 lines of vanilla HTML/CSS/JS, embedded in the binary

No framework, no build step, no node_modules. A chat framework buys nothing
here: the UI is one chat pane, a library sidebar with indexing progress, a
model picker, and a slide-over reader. Assets are compiled into the binary
with `rust-embed`, so the server binary is truly a single file.

### Privacy/offline posture

- Server binds `127.0.0.1` only; no telemetry, no update checks.
- The only network code in the product is the optional first-run model
  download (user-initiated); with the bundled-model installer the app never
  makes a network request at all.

## Crate layout

| crate | role | key deps |
|---|---|---|
| `crates/zimlib` | pure-Rust ZIM reader | memmap2, zstd, xz2 |
| `crates/core` | library/indexing/retrieval/engines | tantivy; llama-cpp-2 (feature `llama`) |
| `crates/server` | localhost HTTP: API, SSE chat, /content, embedded UI | axum, rust-embed |
| `crates/librarian` | headless one-file binary (opens browser) | — |
| `crates/app-tauri` | native shell (own workspace, thin) | tauri 2 |
| `ui/` | vanilla HTML/CSS/JS chat UI | none |

### Library management: drop folder + reference-in-place

Two ways to add books, deliberately both:

- **Managed drop folder** (`<data>/books/`): scanned at startup and on
  demand (`POST /api/library/scan`); any new `.zim` is added and indexed
  automatically. The zero-thought path for non-technical users.
- **Reference in place** (+ Add → file browser): ZIMs can be 100 GB and live
  on external drives or an existing Kiwix folder; forcing a copy into an app
  directory would be hostile, so files added by path are never copied or
  moved. Books whose file has moved show a "missing" badge instead of
  breaking. Browsers deliberately hide native-picker file paths from web
  pages, so the picker is a small server-backed folder navigator
  (`GET /api/fs` — localhost-only, lists directories and `.zim` files only),
  which also works identically inside the Tauri webview.

### Conversations: persistent chats + context-aware retrieval

- Chats persist as one JSON file each under `<data>/chats/` (`ChatStore`);
  assistant messages store the passages they cited, so citations stay
  clickable across restarts. API: `GET/POST /api/chats`,
  `GET/DELETE /api/chats/:id`; `/api/chat` takes `{chat_id?, message}` and
  creates + titles the chat from the first question when no id is given.
- Follow-up questions ("how do I configure **it** at boot?") can't be answered
  by keyword search alone: `contextual_question()` detects anaphoric/short
  questions and folds the previous user turns into the BM25 query, while the
  LLM prompt independently carries a window of prior turns for conversational
  continuity. Self-contained questions pass through untouched.
- Reader UX: on desktop the source panel docks as a flex sibling so answer
  and source sit side by side; under 980 px it becomes a full-screen overlay
  (phone/tablet behavior).

## Verification (what the test-suite covers)

- `zimlib`: parses openzim test-suite files (v5/v6, both namespace schemes),
  reads main page/metadata/content, binary-search lookup.
- `core`: HTML extraction, chunking, prompt construction, stub streaming;
  full e2e — index a real Kiwix ZIM (Alpine Linux wiki), retrieve relevant
  passages for a natural-language question, run a cited chat turn.
- Live server exercised end-to-end (add books → index → SSE chat with sources
  → article serving with highlight injection → asset resolution).

## Requirement flags (honest deviations)

- **Phones/tablets:** iOS and Android projects are generated and working —
  the full pipeline (indexing, retrieval, on-device llama.cpp generation
  with citations) verified in the iOS simulator and on real Android
  hardware. Store submission remains: TestFlight/App Store need the Apple
  account signed into Xcode; Play needs an upload keystore and a one-time
  developer registration. 32-bit Android devices are limited to smaller
  books (address space) and the smallest models.
- **"Download one file"**: the release workflow signs + notarizes macOS
  builds once the `APPLE_*` repo secrets are configured
  (`docs/RELEASING.md`); Windows remains unsigned (SmartScreen warning)
  until an Authenticode cert is added.
- **Strict offline**: desktop installers bundle the model — offline from
  first launch. Mobile/headless installs make user-initiated downloads at
  setup (models + books), then never touch the network again.
- **Hallucination bounds**: grounding is enforced by prompt + retrieval, and
  citations let users verify every claim, but a small LLM can still
  occasionally misstate a source. The extractive mode is the zero-trust
  fallback; a per-sentence "citation-required" post-check is a possible
  future hardening.
