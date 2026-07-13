# ZIM Librarian

**Your offline library, answered.** Ask questions in plain language and get
answers grounded strictly in your local library of [ZIM](https://wiki.openzim.org)
files (Wikipedia, StackExchange, Gutenberg, DevDocs, wikis — anything from
[library.kiwix.org](https://library.kiwix.org)). Every claim carries a
clickable citation that opens the exact source passage, highlighted. After
installation it never touches the network.

- **Works out of the box** — desktop installers ship with **OLMo 2 1B**
  pre-installed; a first-run screen offers a curated starter library
  (Wikipedia's vital-articles selection, the WikiMed medical encyclopedia,
  the OpenStreetMap wiki) as one-click downloads.
- **100% offline** — server binds `127.0.0.1` only; no telemetry, no update
  checks. The network is touched only when *you* ask for a download.
- **One process, one file** — pure-Rust ZIM reader, tantivy BM25 passage index,
  llama.cpp inference, and the chat UI, all in a single binary.
- **Open models** — OLMo 2 1B (fully open weights *and* data) ships by
  default; any `.gguf` works via the catalog, a file picker, or a pasted URL.
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

## Platform status

| platform | status |
|---|---|
| macOS | ✅ native `.app`/`.dmg` (~1 GB, model included); release workflow signs + notarizes once the `APPLE_*` secrets are set (`docs/RELEASING.md`) |
| Windows / Linux | ✅ code is portable (CPU llama.cpp, no OS-specific deps); CI builds and tests on both; installers produced by the release workflow (`.msi`/`.exe`, `.AppImage`/`.deb`), model included |
| iOS | ✅ builds and runs (Tauri 2; `crates/app-tauri/gen/apple`); verified in the iPhone simulator with full on-device LLM answers; device deploys/TestFlight need the Apple account signed into Xcode |
| Android | ✅ builds and runs on real hardware (arm64 + 32-bit armv7); verified on a 2.8 GB-RAM phone with on-device LLM answers; Play upload needs a keystore (`docs/RELEASING.md`) |
| any OS, no install | `cargo build --release --features llama -p librarian` gives a single ~14 MB binary that serves the UI to your browser (models/books download in-app) |

Mobile fine print: models and books are downloaded in-app (stores frown on
multi-GB packages); 32-bit Android devices are limited to smaller books
(< ~300 MB) by their address space, and phone-class hardware wants the
smaller models (OLMo 1B-class).

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

1. **First run**: a welcome screen offers the starter library (general
   knowledge + medicine + maps, pre-checked with sizes shown) — one click
   and it downloads, indexes, and you're ready. The desktop app's model is
   already installed.
2. Add more books any way you like (Library & Model tab):
   - **Starter library**: curated picks, one-click download.
   - **Fetch**: paste any direct `.zim` link from library.kiwix.org.
   - **Drop folder**: put `.zim` files in `<data>/books/` — they're added
     automatically at startup or via **⟳ Rescan**.
   - **+ Add**: browse anywhere on disk and pick a file; it is referenced
     in place (nothing is copied), which is what you want for 100 GB
     Wikipedia dumps on an external drive or an existing Kiwix folder.
   Indexing runs in the background with a progress bar (one-time per book).
3. Swap models any way you like (Model section): curated catalog with
   Download buttons, **+ Add** to pick a `.gguf` anywhere on disk, **Fetch**
   for a pasted URL, or the drop folder. No model at all still works — the
   librarian quotes the most relevant passages verbatim.
4. Ask. Click any numbered chip to open the cited passage, highlighted, in
   the original article.

Data lives in the platform app-data dir (`~/Library/Application Support/zim-librarian`
on macOS); override with `ZIM_LIBRARIAN_DATA`. `ZIM_LIBRARIAN_PORT` pins the
port; `ZIM_LIBRARIAN_NO_BROWSER=1` suppresses browser launch.

## Recommended models

| hardware | model | size | notes |
|---|---|---|---|
| **shipped default** (8 GB machines, phones) | OLMo-2-0425-1B-Instruct Q4_K_M | ~0.9 GB | fully open weights, data and training code; bundled with desktop installers |
| quality upgrade, 8 GB+ | Gemma 4 E2B-it Q4_K_M | ~2.9 GB | best grounding/citation quality tested |
| previous-gen Gemma | Gemma 3n E2B-it Q4_K_M | ~2.8 GB | fallback if Gemma 4 is unsupported by the bundled llama.cpp |
| strongest fully-open, 16 GB+ | OLMo-3-7B-Instruct Q4_K_M | ~4.5 GB | |

In side-by-side testing on the same library, Gemma-class models followed the
cite-only-from-sources instruction natively (every sentence cited, content
faithful to the passages); OLMo-2-1B cites partially and occasionally mixes
in outside knowledge — the retrieval planner's output filter, the triage
pass and citation alignment exist to keep it honest. Hardware below OLMo
1B's comfort zone still works in extractive mode (verbatim passages), and
any tiny `.gguf` can be added manually by those who want one.
