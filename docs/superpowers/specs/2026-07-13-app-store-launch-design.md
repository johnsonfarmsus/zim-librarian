# ZIM Librarian — App Store Launch Design

**Date:** 2026-07-13
**Status:** Approved
**Goal:** Ship ZIM Librarian, licensed AGPL-3.0, as a completed release on the
**Apple App Store**, **Google Play Store**, and **desktop** (macOS/Windows/Linux)
— everything landing together as one coordinated launch.

## Context

The app works end-to-end (v0.2.0): pure-Rust ZIM reader, tantivy BM25 retrieval,
in-process llama.cpp inference, chat UI with enforced citations, first-run setup
with a curated starter library and bundled OLMo 2 1B on desktop. iOS and Android
projects exist under `crates/app-tauri/gen/`. The GitHub repo
(`github.com/johnsonfarmsus/zim-librarian`) is currently **private** and the code
declares **MIT** in Cargo.toml (there is no `LICENSE` file).

What stands between here and "in the stores" is not features — it is licensing,
compliance artifacts, store paperwork, signing material, and one genuine
technical unknown: on-device generative inference has never been verified on real
hardware (iOS was simulator-only on a CPU path; Android was verified once on a
single low-end 32-bit phone).

## The one real risk

**On-device generative inference on real hardware.** Everything else is
mechanical. iOS Metal inference on a physical iPhone has never produced an answer
(the simulator used a CPU-only path because Metal stalls there). Android ran on
one 2.8 GB 32-bit phone at ~19 min/answer. The plan therefore front-loads a
**verification gate**: get a real answer out of a real iPhone and a modern arm64
Android phone *before* investing in store paperwork, so that if Metal misbehaves
we discover it early — not after building icons, screenshots, and listings.

## Licensing decision (resolved)

- **AGPL-3.0-only**, with a short **App Store distribution exception** appended to
  the license grant. The user is the **sole copyright holder**, so:
  - Publishing their own AGPL work to the Apple App Store is fine — the AGPL binds
    *licensees*, not the copyright holder. The VLC-style App-Store/GPL conflict
    only bites when third-party AGPL code is present.
  - The exception is **future-proofing**: the day a first outside contributor's
    code is merged, the exception preserves the right to distribute via Apple.
- **Source linkback (AGPL §13):** a visible "Source code" link in the app UI and
  in the docs, pointing at the public GitHub repo. This requires the repo to be
  **public** (approved, pending the completed security audit — which passed).

## Security audit (passed, 2026-07-13)

No tracked secret files, no secrets in git history, no hardcoded keys/tokens/
passwords, no large model/ZIM blobs committed, no personal machine paths. The
`release.yml` workflow reads Apple secrets from Actions env vars (no embedded
values). Non-blocking notes: commit author email and the `us.johnsonfarms.zimlibrarian`
bundle ID are intended-public identifiers; the Android upload keystore generated
later **must be gitignored and never committed**. Conclusion: **safe to go public.**

## Workstreams

### A. AGPL relicense + compliance
- Add top-level `LICENSE` with the full **AGPL-3.0** text plus an **App Store
  distribution exception** clause.
- Change every `license = "MIT"` → `license = "AGPL-3.0-only"` (workspace root
  `Cargo.toml` and `crates/app-tauri/Cargo.toml`; the other crates use
  `license.workspace = true` and inherit).
- Add a **License** section to `README.md`.
- Wire a visible **"Source code"** link into the app UI (footer/About) and the
  mobile menu, pointing at the public repo — satisfies AGPL §13.
- **Flip the GitHub repo to public** (gated on the passed audit).

### B. Legal / policy pages
- **Privacy policy** — honest and short: no data collected, no telemetry, network
  touched only for user-initiated downloads. Hosted free on **GitHub Pages** from
  the repo.
- **Support page** — contact / issues pointer, also on GitHub Pages.
- Both URLs are required by both stores and double as the AGPL source-pointer home.

### C. Branding & assets (generated in-repo)
- **App icon** — a book/library motif authored as SVG, rendered to:
  - iOS `AppIcon.appiconset` (all required sizes).
  - Android adaptive icon (foreground + background layers, mipmap densities).
- **Google Play feature graphic** (1024×500).
- **Screenshots** captured from the real running app on each platform, framed to
  each store's required device sizes. (Depends on the verification gate putting a
  working build on real devices.)

### D. iOS App Store submission
- Register bundle ID `us.johnsonfarms.zimlibrarian` at developer.apple.com.
- Create the App Store Connect app record.
- Automatic signing via the user's account in Xcode (distribution cert +
  provisioning profile).
- Release **archive** → **App Privacy** labels ("Data Not Collected") → upload →
  **TestFlight** (test on own device, no review) → submit for **App Review**.
- Category: Reference / Education. Metadata (name, subtitle, description,
  keywords, age rating) drafted for user approval; user performs account-level
  clicks (Claude does not log into the account).

### E. Google Play submission
- **Generate an upload keystore** (Claude creates it; **user stores it safely** —
  loss is unrecoverable; it is gitignored, never committed).
- Wire real release signing into `gen/android/app/build.gradle.kts` (replacing the
  current debug-signed release config).
- Build an **arm64 `.aab`** (Android App Bundle).
- Play Console: store listing, **Data Safety** form (no data collected), **IARC
  content rating**, target audience, privacy-policy URL.
- **Closed testing: 12 testers for 14 continuous days** (mandatory for newer
  personal developer accounts) → then production. This is the schedule long pole;
  its clock starts as early as possible.
- **Tester sourcing:** the user has 2 Android phones + household members for
  genuine on-device testing, but needs 12 opted-in **Google accounts**. Plan:
  household/friends create free Google accounts and **opt in via the web link**
  (an Android device is not strictly required to be a counted tester, though
  on-device testing is better); fill any remainder via a **mutual tester-exchange
  community**. Recruiting the 12 is the first Android action so the 14-day clock
  starts immediately.

### F. Desktop signed release
- **macOS**: Developer ID signing + **notarization** via the `APPLE_*` GitHub
  Actions secrets the `release.yml` workflow already expects (steps in
  `docs/RELEASING.md`).
- **Windows**: **ship unsigned at launch** with a documented SmartScreen note.
  Pursue **SignPath Foundation** (free OV code signing for qualifying OSS,
  HSM-hosted, integrates into the existing GitHub Actions release workflow) as a
  **fast-follow** — its external vetting/approval has a lead time we do not
  control, so it must **not gate the coordinated launch**; swap the CI signing
  step in once approved (re-signs the installer, no app changes).
  **Not** pursuing the Microsoft Store / MSIX route for this launch — it is a
  separate certification + submission pipeline (a fourth store) that cuts against
  one coordinated launch; it remains an optional additional channel for later.
- **Linux**: `.AppImage` / `.deb` (unaffected by signing).
- Tag the release version → workflow builds signed (macOS) / unsigned (Windows) /
  packaged (Linux) installers → publish the GitHub release.

## Sequence

0. **A + B** — relicense, compliance, privacy/support pages, repo public. Low risk,
   unblocks truthful docs and the in-app source link.
1. **Verification gate** — real answer on a physical iPhone (Metal) and a modern
   arm64 Android phone. Resolve the only technical unknown before paperwork.
2. **C** — icon can be done anytime; screenshots after step 1 yields running builds.
3. **D** (iOS) and **F** (desktop) proceed; **E** (Android) starts its 12-tester
   clock ASAP so it runs in parallel.
4. **Coordinated launch** — desktop GitHub release published, both store
   submissions in review/live, everything landing together.

## What is needed from the user (just-in-time, not all now)

- **Now:** go-ahead to flip the repo public (audit passed).
- **Apple:** Team ID; confirmation the Apple account is signed into Xcode on this
  Mac; the user clicks submit/agree buttons in App Store Connect.
- **Google:** confirmation of the Play Console account; a safe place to keep the
  generated upload keystore; **~12 people** to opt into the 14-day closed test.
- **Hardware:** a physical iPhone and a modern arm64 Android phone for the
  verification gate.

## Out of scope

- Mac App Store (sandbox conflicts with reference-in-place ZIM files anywhere on
  disk — notarized direct download is the macOS channel).
- Background/headless mobile inference (foreground-only localhost architecture is
  accepted).
- 32-bit Android as a first-class target (arm64 `.aab` for v1; 32-bit remains a
  documented best-effort).

## Verification / done criteria

- `cargo test --workspace` green throughout.
- AGPL: `LICENSE` present, all crates declare AGPL, in-app source link visible,
  repo public.
- Privacy + support pages live at stable GitHub Pages URLs.
- A generative answer produced on a physical iPhone and a physical arm64 Android
  phone.
- iOS build in TestFlight, then submitted for review.
- Android `.aab` in Play closed testing, then submitted for production.
- Desktop: signed + notarized macOS artifact (`spctl -a -vv` passes), published
  GitHub release with all platform installers.
