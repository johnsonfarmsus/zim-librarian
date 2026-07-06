# ZIM Librarian

**Your offline library, answered.** Ask questions in plain language and get
answers grounded strictly in your local library of [ZIM](https://wiki.openzim.org)
files (Wikipedia, StackExchange, Gutenberg, DevDocs, wikis — anything from
[library.kiwix.org](https://library.kiwix.org)). Every claim carries a
clickable citation that opens the exact source passage, highlighted. After
installation it never touches the network.

- **100% offline** — server binds `127.0.0.1` only; no telemetry, no update checks.
- **One process, one file** — pure-Rust ZIM reader, tantivy BM25 passage index,
  llama.cpp inference, and the chat UI, all in a single binary.
- **Open models** — any `.gguf` works; OLMo-2-Instruct recommended
  (fully open weights *and* data), SmolLM2/Qwen-class for weak hardware,
  Gemma for those who prefer it.
- **Citations that can't be skipped** — if the model forgets to cite, an
  alignment pass attaches each supported sentence to its best source; text the
  sources don't support stays visibly uncited.

See [`docs/PLAN.md`](docs/PLAN.md) for the full technical plan and the
reasoning behind every tooling choice.

## Repository layout

| path | what it is |
|---|---|
| `crates/zimlib` | minimal pure-Rust ZIM reader (v5/v6, old+new namespaces, xz/zstd) |
| `crates/core` | library manager, HTML→text, chunking, tantivy index, retrieval, engines |
| `crates/server` | localhost HTTP: JSON API, SSE chat, `/content/<zim>/<path>` article serving, embedded UI |
| `crates/librarian` | one-file headless binary: starts the server, opens your browser |
| `crates/app-tauri` | Tauri 2 native shell (desktop + mobile); own workspace, thin wrapper |
| `ui/` | vanilla HTML/CSS/JS chat frontend (no build step) |

## Build & run (developers)

```sh
# headless binary with local LLM support (needs cmake for llama.cpp):
cargo build --release --features llama -p librarian
./target/release/librarian          # serves 127.0.0.1:<port>, opens browser

# native desktop app:
cd crates/app-tauri && cargo tauri build     # or: cargo run (dev window)

# tests (uses real ZIM files in testdata/):
cargo test
```

Test fixtures: `testdata/` holds files from the
[openzim zim-testing-suite](https://github.com/openzim/zim-testing-suite) plus
two small real Kiwix ZIMs; re-download with the URLs in `docs/PLAN.md` history
or any small ZIM from library.kiwix.org.

## Using it

1. Download `.zim` files (e.g. from library.kiwix.org) onto the machine.
2. Add books either way (Library & Model tab):
   - **Drop folder**: put `.zim` files in `<data>/books/` — they're added
     automatically at startup or via **⟳ Rescan**. Simplest for most users.
   - **+ Add**: browse anywhere on disk and pick a file; it is referenced
     in place (nothing is copied), which is what you want for 100 GB
     Wikipedia dumps on an external drive or an existing Kiwix folder.
   Indexing runs in the background with a progress bar (one-time per book).
3. Drop a `.gguf` model into the models folder shown in the sidebar (or use
   the extractive no-model mode, which quotes passages verbatim).
4. Ask. Click any numbered chip to open the cited passage, highlighted, in
   the original article.

Data lives in the platform app-data dir (`~/Library/Application Support/zim-librarian`
on macOS); override with `ZIM_LIBRARIAN_DATA`. `ZIM_LIBRARIAN_PORT` pins the
port; `ZIM_LIBRARIAN_NO_BROWSER=1` suppresses browser launch.

## Recommended models

| hardware | model | size | notes |
|---|---|---|---|
| **default: 8 GB+ laptops, tablets, recent phones** | Gemma 3n E2B-it Q4_K_M | ~2.8 GB | best grounding/citation quality tested; designed for on-device |
| full transparency (open data + code) | OLMo-2-0425-1B-Instruct Q4_K_M | ~0.9 GB | fully open; decent but occasionally imports outside knowledge |
| stronger open option, 16 GB+ | OLMo-2-1124-7B-Instruct Q4_K_M | ~4.5 GB | |
| very old machines / minimal | SmolLM2-360M-Instruct Q8 | ~0.4 GB | relies on the citation-alignment pass; treat as search + summaries |

In side-by-side testing on the same library, Gemma 3n E2B followed the
cite-only-from-sources instruction natively (every sentence cited, content
faithful to the passages); OLMo-2-1B cited partially and occasionally mixed
in outside knowledge; SmolLM2-360M needs the always-on citation-alignment
pass and is best treated as "search with summaries".
