# ZIM Librarian App Store Launch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship ZIM Librarian, relicensed AGPL-3.0, as a completed release on the Apple App Store, Google Play Store, and desktop (macOS notarized / Windows / Linux), landing together as one coordinated launch.

**Architecture:** The app is unchanged — this plan adds the licensing, compliance, branding, signing, and store-submission scaffolding around it. Code/config tasks use tight verify loops; store and account tasks are explicit checklists. A **verification gate** (real on-device generative answer, iOS + Android) runs before any store paperwork, because on-device inference is the only untested surface.

**Tech Stack:** Rust workspace + Tauri 2 (iOS `gen/apple`, Android `gen/android`); `cargo tauri` CLI; GitHub Actions `release.yml`; GitHub Pages for policy docs; `keytool`/`jarsigner` for Android signing; Xcode + App Store Connect for iOS.

## Global Constraints

- **License:** `AGPL-3.0-only`, applied to workspace root `Cargo.toml` and `crates/app-tauri/Cargo.toml` (other crates inherit via `license.workspace = true`).
- **App Store exception:** an AGPL §7 additional-permission clause is prepended to `LICENSE`, granting distribution via app stores (future-proofs the first outside contributor).
- **Source linkback (AGPL §13):** the repo URL `https://github.com/johnsonfarmsus/zim-librarian` must appear as a visible link in the app UI and in docs.
- **Privacy stance:** no data collected, no telemetry, network touched only on user-initiated downloads — every store form and policy page states exactly this.
- **Bundle ID / package:** `us.johnsonfarms.zimlibrarian` (already set on both platforms — do not change).
- **Android artifact:** arm64 `.aab` (Android App Bundle) for v1; 32-bit is documented best-effort only.
- **Keystore:** the Android upload keystore is **gitignored and never committed**; the user stores it safely (loss is unrecoverable).
- **Release version:** bump `0.2.0` → **`1.0.0`** in all three lockstep spots (root `Cargo.toml`, `crates/app-tauri/Cargo.toml`, `tauri.conf.json`).
- **Safety boundary:** Claude does **not** log into the user's Apple/Google accounts, enter credentials, accept store terms, or click irreversible submit buttons. Those steps are marked **USER ACTION**; Claude prepares everything up to that line.
- **Out of scope:** Mac App Store; Microsoft Store/MSIX; first-class 32-bit Android; background mobile inference.

---

## Phase 0 — Licensing & compliance

### Task 1: AGPL-3.0 LICENSE file with App Store exception

**Files:**
- Create: `LICENSE`

- [ ] **Step 1: Fetch the canonical AGPL-3.0 text**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
curl -fsSL https://www.gnu.org/licenses/agpl-3.0.txt -o LICENSE
head -1 LICENSE   # expect: "                    GNU AFFERO GENERAL PUBLIC LICENSE"
```

- [ ] **Step 2: Prepend the exception preamble**

Prepend this block to the very top of `LICENSE` (above the fetched text), then a blank line and a line of `----`:

```
ZIM Librarian
Copyright (C) 2026 Trevor Johnson (JohnsonFarms.us)

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License, version 3, as published by
the Free Software Foundation, with the following additional permission under
section 7:

  ADDITIONAL PERMISSION. The copyright holder grants permission to distribute
  official builds of this Program through application distribution platforms
  (including the Apple App Store and Google Play Store) under those platforms'
  standard distribution terms, notwithstanding any conflict between those terms
  and the terms of the GNU Affero General Public License. This additional
  permission covers distribution of official builds by the copyright holder and
  does not restrict any right granted to recipients by the AGPL, including the
  right to the corresponding source code.

The full text of the GNU Affero General Public License, version 3, follows.

--------------------------------------------------------------------------------
```

- [ ] **Step 3: Verify**

```bash
grep -c "ADDITIONAL PERMISSION" LICENSE   # expect: 1
grep -c "AFFERO" LICENSE                  # expect: >= 1
```
Expected: `1` then a non-zero count.

- [ ] **Step 4: Commit**

```bash
rm -f .git/index.lock
git add LICENSE
git commit -m "Add AGPL-3.0 license with app-store distribution exception"
```

---

### Task 2: Switch Cargo license fields to AGPL-3.0-only

**Files:**
- Modify: `Cargo.toml` (workspace root, `license = "MIT"` → AGPL)
- Modify: `crates/app-tauri/Cargo.toml` (`license = "MIT"` → AGPL)

**Interfaces:**
- Produces: all crates report `AGPL-3.0-only` in `cargo metadata`.

- [ ] **Step 1: Edit root `Cargo.toml`**

Under `[workspace.package]`, change `license = "MIT"` to:
```toml
license = "AGPL-3.0-only"
```

- [ ] **Step 2: Edit `crates/app-tauri/Cargo.toml`**

In `[package]`, change `license = "MIT"` to:
```toml
license = "AGPL-3.0-only"
```

- [ ] **Step 3: Verify no MIT remains and metadata is valid**

```bash
grep -rn 'license = "MIT"' Cargo.toml crates/*/Cargo.toml crates/app-tauri/Cargo.toml   # expect: no output
cargo metadata --no-deps --format-version 1 >/dev/null && echo OK   # expect: OK
```
Expected: no MIT hits, then `OK`.

- [ ] **Step 4: Commit**

```bash
rm -f .git/index.lock
git add Cargo.toml crates/app-tauri/Cargo.toml
git commit -m "Relicense crates MIT -> AGPL-3.0-only"
```

---

### Task 3: README license section + fix stale About copy

**Files:**
- Modify: `README.md` (add License section at end)
- Modify: `ui/index.html` (About dialog says "Starter library" — the sections were merged into "Library")

- [ ] **Step 1: Append a License section to `README.md`**

```markdown
## License

ZIM Librarian is free software under the **GNU AGPL-3.0**, with an additional
permission (AGPL §7) allowing distribution of official builds through the Apple
App Store and Google Play Store. See [`LICENSE`](LICENSE). The complete
corresponding source is this repository:
<https://github.com/johnsonfarmsus/zim-librarian>.
```

- [ ] **Step 2: Fix the stale About copy in `ui/index.html`**

In the About dialog "Adding books" paragraph, change `The <b>Starter library</b> section offers` to:
```html
The <b>Library</b> tab offers a curated set
```

- [ ] **Step 3: Verify**

```bash
grep -c "AGPL-3.0" README.md            # expect: >= 1
grep -c "Starter library" ui/index.html # expect: 0
```

- [ ] **Step 4: Commit**

```bash
rm -f .git/index.lock
git add README.md ui/index.html
git commit -m "README license section; fix stale About copy"
```

---

### Task 4: Visible in-app "Source code" link (AGPL §13)

**Files:**
- Modify: `ui/index.html:71` (footer) and the About-dialog Privacy paragraph

- [ ] **Step 1: Add the source link to the footer**

Replace line 71:
```html
    <div class="footer hint">100% offline · answers cite your books</div>
```
with:
```html
    <div class="footer hint">100% offline · answers cite your books ·
      <a href="https://github.com/johnsonfarmsus/zim-librarian" target="_blank" rel="noopener">source (AGPL-3.0)</a></div>
```

- [ ] **Step 2: Add source line to the About Privacy paragraph**

At the end of the About "Privacy" paragraph, append:
```html
        This app is free software (AGPL-3.0); its complete source code is at
        <a href="https://github.com/johnsonfarmsus/zim-librarian" target="_blank" rel="noopener">github.com/johnsonfarmsus/zim-librarian</a>.
```

- [ ] **Step 3: Verify the link is served**

```bash
grep -c "github.com/johnsonfarmsus/zim-librarian" ui/index.html   # expect: 2
```
Expected: `2`.

- [ ] **Step 4: Visual check (manual)**

Run `cargo build --release --features llama -p librarian && ./target/release/librarian`, open the browser, confirm the "source (AGPL-3.0)" link shows in the sidebar footer and the About dialog. Stop the server.

- [ ] **Step 5: Commit**

```bash
rm -f .git/index.lock
git add ui/index.html
git commit -m "Add visible source-code link (AGPL section 13)"
```

---

### Task 5: Privacy policy + support pages (GitHub Pages)

**Files:**
- Create: `docs/site/index.md`
- Create: `docs/site/privacy.md`
- Create: `docs/site/support.md`
- Create: `docs/site/_config.yml`

**Interfaces:**
- Produces: stable URLs once Pages is enabled (Task 6): `https://johnsonfarmsus.github.io/zim-librarian/privacy` and `.../support`. These URLs are consumed by the iOS (Task 14) and Android (Task 17) store listings.

- [ ] **Step 1: Create `docs/site/_config.yml`**

```yaml
title: ZIM Librarian
description: Your offline library, answered.
theme: jekyll-theme-minimal
```

- [ ] **Step 2: Create `docs/site/index.md`**

```markdown
# ZIM Librarian

A private, fully offline AI research assistant for your Kiwix ZIM library.
Free software under the [GNU AGPL-3.0](https://github.com/johnsonfarmsus/zim-librarian/blob/main/LICENSE).

- [Source code](https://github.com/johnsonfarmsus/zim-librarian)
- [Privacy policy](privacy)
- [Support](support)
```

- [ ] **Step 3: Create `docs/site/privacy.md`**

```markdown
# Privacy Policy

_Last updated: 2026-07-13_

ZIM Librarian is designed to be fully offline and private.

- **No data collection.** The app collects no personal data, no analytics, and
  no telemetry of any kind.
- **No accounts.** There is no sign-in and no user account.
- **On-device only.** Your questions, chats, and documents stay on your device.
  The app's server binds to `127.0.0.1` (loopback) and never exposes your data.
- **Network use.** The app accesses the network **only** when you explicitly ask
  it to download a model or a ZIM book from a source you choose. Nothing else is
  transmitted.
- **No third-party sharing.** Because no data is collected, none is shared.

Questions: file an issue at
<https://github.com/johnsonfarmsus/zim-librarian/issues>.
```

- [ ] **Step 4: Create `docs/site/support.md`**

```markdown
# Support

ZIM Librarian is maintained on GitHub.

- **Report a bug or ask a question:**
  <https://github.com/johnsonfarmsus/zim-librarian/issues>
- **Source code & documentation:**
  <https://github.com/johnsonfarmsus/zim-librarian>

There is no telemetry, so please include your platform and app version when
reporting an issue.
```

- [ ] **Step 5: Verify**

```bash
ls docs/site/{index.md,privacy.md,support.md,_config.yml}   # all four listed
grep -c "No data collection" docs/site/privacy.md           # expect: 1
```

- [ ] **Step 6: Commit**

```bash
rm -f .git/index.lock
git add docs/site
git commit -m "Add privacy + support pages for GitHub Pages"
```

---

### Task 6: Make repo public + enable GitHub Pages — **USER ACTION**

**Files:** none (GitHub settings).

- [ ] **Step 1: Push all Phase 0 commits**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
git push origin main
```
If the push hangs (iCloud eviction), run `git repack -adq` first, then retry.

- [ ] **Step 2: USER — flip repo visibility to public**

GitHub → repo **Settings** → **General** → **Danger Zone** → **Change repository visibility** → **Public**. (Confirm to Claude when done — this is the AGPL source-availability requirement and makes the in-app link resolve.)

- [ ] **Step 3: USER — enable GitHub Pages**

Settings → **Pages** → Source: **Deploy from a branch** → Branch: `main`, folder: `/docs/site` → Save. Wait ~1 min.

- [ ] **Step 4: Verify Pages is live**

```bash
curl -fsSL https://johnsonfarmsus.github.io/zim-librarian/privacy | grep -c "Privacy Policy"   # expect: 1
```
Expected: `1`. (Note the actual published URL; store listings link to it.)

---

## Phase 1 — On-device verification gate (the only technical unknown)

### Task 7: iOS device — build, run, verify a generative answer

**Files:** none (build + observe). Prereq: an iPhone; Apple account signed into Xcode.

- [ ] **Step 1: Free disk (this Mac runs tight) and confirm targets**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
rustup target list --installed | grep -E 'aarch64-apple-ios$' || rustup target add aarch64-apple-ios
df -h / | tail -1   # ensure several GB free before building
```

- [ ] **Step 2: USER — connect iPhone, trust the Mac, select the team in Xcode**

Open `crates/app-tauri/gen/apple/*.xcodeproj` in Xcode once; under Signing & Capabilities pick your Team (automatic signing). Close Xcode.

- [ ] **Step 3: Build + run on device**

```bash
cd crates/app-tauri
cargo tauri ios build --target aarch64
```
Then in Xcode press Run on the connected device (or `cargo tauri ios dev --host` for a live run). Expected: app launches on the phone.

- [ ] **Step 4: Verify a real generative answer (the gate)**

On the phone: complete first-run setup, download the OLMo 2 1B model + the smallest starter book (OSM wiki), ask a question, and confirm a **generated, cited answer** appears (not just extractive passages). Note wall-clock time.

- [ ] **Step 5: Record the result**

If it works: note timing, proceed. If Metal misbehaves (crash/stall/garbage), STOP and report — set `n_gpu_layers = 0` for device as a fallback (CPU-only) in `crates/core/src/llama.rs` and re-test before continuing. This gate decides whether iOS ships.

---

### Task 8: Android arm64 device — build, run, verify a generative answer

**Files:** none (build + observe). Prereq: a modern arm64 Android phone.

- [ ] **Step 1: Environment + target**

```bash
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK="$ANDROID_HOME/ndk/27.2.12479018"
export ANDROID_NDK_ROOT="$ANDROID_NDK"; export NDK_HOME="$ANDROID_NDK"
export JAVA_HOME="$(/usr/libexec/java_home)"
rustup target list --installed | grep -E 'aarch64-linux-android$' || rustup target add aarch64-linux-android
```

- [ ] **Step 2: Build + install on the arm64 device**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian/crates/app-tauri
cargo tauri android build --target aarch64 --apk
```
Install the produced APK (`adb install -r gen/android/app/build/outputs/apk/**/app-*-release.apk`) on the connected phone.

- [ ] **Step 3: Verify a real generative answer (the gate)**

On the phone: first-run setup → download OLMo 2 1B + OSM wiki → ask a question → confirm a generated, cited answer. Note timing (arm64 should be far faster than the ~19 min seen on the old 32-bit phone).

- [ ] **Step 4: Record the result**

If it works, proceed. If not, report before continuing — Android shipping depends on this gate.

---

## Phase 2 — Branding & assets

### Task 9: App icon — design, rasterize master, fan out to all platforms

**Files:**
- Create: `assets/icon.svg` (source of truth)
- Create: `assets/icon-1024.png` (master)
- Generated: `crates/app-tauri/icons/*`, plus iOS/Android icon sets via `cargo tauri icon`

- [ ] **Step 1: Author `assets/icon.svg`**

A 1024×1024 flat book/library mark on a solid rounded background (no transparency — iOS forbids alpha in the app icon). Example scaffold (refine visually):
```xml
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <rect width="1024" height="1024" fill="#1f6f6b"/>
  <g fill="#f5f1e6">
    <rect x="300" y="270" width="180" height="470" rx="14"/>
    <rect x="500" y="230" width="180" height="510" rx="14"/>
    <rect x="700" y="300" width="120" height="440" rx="14" transform="rotate(9 760 520)"/>
  </g>
  <rect x="270" y="740" width="500" height="40" rx="20" fill="#123f3c"/>
</svg>
```

- [ ] **Step 2: Rasterize to a 1024×1024 PNG master**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
qlmanage -t -s 1024 -o assets assets/icon.svg && mv assets/icon.svg.png assets/icon-1024.png
sips -g pixelWidth -g pixelHeight assets/icon-1024.png   # expect 1024 x 1024
```
If width ≠ 1024 (qlmanage padding), fallback: open the SVG in Chrome at 1024², screenshot the tab, save as `assets/icon-1024.png`. Re-run the `sips` check.

- [ ] **Step 3: Generate all platform icons**

```bash
cd crates/app-tauri
cargo tauri icon ../../assets/icon-1024.png
```
Expected: writes `icons/` (desktop `.icns`/`.ico`/pngs) and updates the iOS `AppIcon.appiconset` and Android `mipmap-*` sets.

- [ ] **Step 4: Verify icon sets landed**

```bash
ls crates/app-tauri/icons/icon.icns crates/app-tauri/icons/icon.ico
ls crates/app-tauri/gen/apple/*/Assets.xcassets/AppIcon.appiconset/*.png | head
ls crates/app-tauri/gen/android/app/src/main/res/mipmap-xxxhdpi/*.png | head
```
Expected: files present in all three.

- [ ] **Step 5: Commit**

```bash
rm -f .git/index.lock
git add assets crates/app-tauri/icons crates/app-tauri/gen/apple crates/app-tauri/gen/android
git commit -m "Add app icon and generate platform icon sets"
```

---

### Task 10: Google Play feature graphic (1024×500)

**Files:**
- Create: `assets/play-feature.svg`, `assets/play-feature-1024x500.png`

- [ ] **Step 1: Author `assets/play-feature.svg`** (1024×500, app name + tagline "Your offline library, answered." on the brand background, icon mark at left).

- [ ] **Step 2: Rasterize**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
qlmanage -t -s 1024 -o assets assets/play-feature.svg && mv assets/play-feature.svg.png assets/play-feature-1024x500.png
sips -g pixelWidth -g pixelHeight assets/play-feature-1024x500.png   # expect 1024 x 500
```
(If aspect is wrong via qlmanage, use the Chrome-screenshot fallback at exactly 1024×500.)

- [ ] **Step 3: Commit**

```bash
rm -f .git/index.lock
git add assets/play-feature.svg assets/play-feature-1024x500.png
git commit -m "Add Google Play feature graphic"
```

---

### Task 11: Store screenshots (both platforms)

**Files:**
- Create: `assets/screenshots/ios/*.png`, `assets/screenshots/android/*.png`

- [ ] **Step 1: Capture from the real running app** (from Phase 1 builds): the chat with a cited answer, the reader overlay with a highlighted passage, and the Library tab. On iPhone use the device screenshot; on Android use `adb exec-out screencap -p > shot.png`. Capture at native device resolution.

- [ ] **Step 2: Verify counts/sizes**

```bash
ls assets/screenshots/ios/*.png assets/screenshots/android/*.png
# iOS: >= 3 shots at a supported size (e.g. 1290x2796 for 6.7"); Android: >= 3, min 1080px on the short edge.
sips -g pixelWidth -g pixelHeight assets/screenshots/ios/*.png
```

- [ ] **Step 3: Commit**

```bash
rm -f .git/index.lock
git add assets/screenshots
git commit -m "Add store screenshots"
```

---

## Phase 3 — iOS App Store submission

### Task 12: iOS release config — version, privacy strings, marketing version

**Files:**
- Modify: `Cargo.toml`, `crates/app-tauri/Cargo.toml`, `crates/app-tauri/tauri.conf.json` (version 0.2.0 → 1.0.0)
- Modify: `crates/app-tauri/gen/apple/*/Info.plist` (confirm privacy usage strings)

- [ ] **Step 1: Bump version in all three lockstep spots to `1.0.0`**

Root `Cargo.toml` `[workspace.package] version`, `crates/app-tauri/Cargo.toml` `[package] version`, `tauri.conf.json` `"version"`.

- [ ] **Step 2: Confirm Info.plist privacy + ATS keys**

Ensure the iOS `Info.plist` has `NSAllowsLocalNetworking` (under `NSAppTransportSecurity`), `UIFileSharingEnabled`, and `LSSupportsOpeningDocumentsInPlace` (already added). No camera/location/mic keys should be present (the app uses none).

- [ ] **Step 3: Verify version consistency**

```bash
grep -h '"version"\|^version' crates/app-tauri/tauri.conf.json Cargo.toml crates/app-tauri/Cargo.toml
```
Expected: `1.0.0` in all three.

- [ ] **Step 4: Commit**

```bash
rm -f .git/index.lock
git add Cargo.toml crates/app-tauri/Cargo.toml crates/app-tauri/tauri.conf.json crates/app-tauri/gen/apple
git commit -m "Bump to 1.0.0; confirm iOS privacy/ATS keys"
```

---

### Task 13: Apple developer setup — bundle ID, app record, signing — **USER ACTION**

**Files:** none (Apple portals). Claude drafts values; user performs account actions.

- [ ] **Step 1: USER — provide your Apple **Team ID***

From developer.apple.com → Membership. (Claude needs it only to reference in docs; it is not secret.)

- [ ] **Step 2: USER — register the App ID**

developer.apple.com → Identifiers → `+` → App IDs → Bundle ID **`us.johnsonfarms.zimlibrarian`** (Explicit). Capabilities: leave defaults (no special entitlements needed).

- [ ] **Step 3: USER — create the App Store Connect record**

appstoreconnect.apple.com → Apps → `+` → New App: Platform iOS, Name **ZIM Librarian**, Primary language English (U.S.), Bundle ID `us.johnsonfarms.zimlibrarian`, SKU `zim-librarian-1`.

- [ ] **Step 4: Confirm automatic signing works** (from Task 7 the team is already selected). No manual cert export needed for App Store distribution — Xcode manages it.

---

### Task 14: Archive, upload, TestFlight, submit — **USER ACTION (Claude drafts metadata)**

**Files:**
- Create: `docs/store/ios-listing.md` (Claude-drafted metadata for the user to paste)

- [ ] **Step 1: Claude — draft the listing** into `docs/store/ios-listing.md`: app name, subtitle (≤30 chars), promotional text, description (offline, cited answers, open models, AGPL, no data collected), keywords (≤100 chars: `offline,wikipedia,kiwix,zim,ai,research,library,reference,llm,private`), support URL (`https://johnsonfarmsus.github.io/zim-librarian/support`), privacy policy URL (`.../privacy`), category Reference (secondary Education), copyright `© 2026 Trevor Johnson`. Commit it.

- [ ] **Step 2: USER — App Privacy in App Store Connect**

App → App Privacy → **Data Not Collected** (answer "No, we do not collect data").

- [ ] **Step 3: Build the release archive + upload**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian/crates/app-tauri
cargo tauri ios build --target aarch64
```
Then in Xcode: Product → Archive → Distribute App → App Store Connect → Upload. (Or `xcrun altool`/Transporter with the `.ipa`.)

- [ ] **Step 4: USER — TestFlight self-test**

App Store Connect → TestFlight → install on your own device via the TestFlight app; confirm it runs and answers.

- [ ] **Step 5: USER — attach build, paste metadata + screenshots, Submit for Review**

Fill the version with the drafted metadata and `assets/screenshots/ios`, select the uploaded build, answer the export-compliance question (uses only standard encryption → typically "No" for the exemption prompt), and **Submit for Review**.

---

## Phase 4 — Google Play submission

### Task 15: Android upload keystore + release signing

**Files:**
- Create: `crates/app-tauri/gen/android/keystore/upload.jks` (**gitignored**)
- Modify: `crates/app-tauri/gen/android/app/build.gradle.kts` (add `signingConfigs.release`, point release build at it)
- Modify: `.gitignore` (ignore keystores)

- [ ] **Step 1: Add keystore ignore rule to `.gitignore`**

```
crates/app-tauri/gen/android/keystore/
*.jks
*.keystore
```

- [ ] **Step 2: Generate the upload keystore** (USER keeps the passwords)

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian/crates/app-tauri/gen/android
mkdir -p keystore
keytool -genkeypair -v -keystore keystore/upload.jks -alias upload \
  -keyalg RSA -keysize 2048 -validity 10000
# Choose a strong store+key password when prompted; RECORD THEM SAFELY.
```

- [ ] **Step 3: Wire release signing into `build.gradle.kts`**

Add inside the `android { }` block (before `buildTypes`):
```kotlin
    signingConfigs {
        create("release") {
            storeFile = file(System.getenv("ZIML_KEYSTORE") ?: "keystore/upload.jks")
            storePassword = System.getenv("ZIML_KEYSTORE_PASSWORD")
            keyAlias = System.getenv("ZIML_KEY_ALIAS") ?: "upload"
            keyPassword = System.getenv("ZIML_KEY_PASSWORD")
        }
    }
```
Then in `buildTypes { getByName("release") { ... } }` replace:
```kotlin
            signingConfig = signingConfigs.getByName("debug")
```
with:
```kotlin
            signingConfig = signingConfigs.getByName("release")
```

- [ ] **Step 4: Verify the keystore is ignored**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
git status --porcelain crates/app-tauri/gen/android/keystore/   # expect: no output (ignored)
git check-ignore crates/app-tauri/gen/android/keystore/upload.jks   # expect: the path echoed
```

- [ ] **Step 5: Commit (gradle + gitignore only — never the keystore)**

```bash
rm -f .git/index.lock
git add .gitignore crates/app-tauri/gen/android/app/build.gradle.kts
git commit -m "Android release signing config (upload keystore, gitignored)"
```

---

### Task 16: Build the arm64 `.aab` and verify its signature

**Files:** none (build output).

- [ ] **Step 1: Export signing env + build the bundle**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian/crates/app-tauri
export ANDROID_HOME="$HOME/Library/Android/sdk" ANDROID_NDK="$HOME/Library/Android/sdk/ndk/27.2.12479018"
export ANDROID_NDK_ROOT="$ANDROID_NDK" NDK_HOME="$ANDROID_NDK" JAVA_HOME="$(/usr/libexec/java_home)"
export ZIML_KEYSTORE="$PWD/gen/android/keystore/upload.jks"
export ZIML_KEYSTORE_PASSWORD='...' ZIML_KEY_ALIAS=upload ZIML_KEY_PASSWORD='...'
cargo tauri android build --target aarch64 --aab
```

- [ ] **Step 2: Locate + verify the signed bundle**

```bash
AAB=$(ls gen/android/app/build/outputs/bundle/*/app-*.aab | head -1); echo "$AAB"
jarsigner -verify -verbose -certs "$AAB" | grep -m1 -i "jar verified" && echo SIGNED
```
Expected: the `.aab` path, then `jar verified` / `SIGNED`.

- [ ] **Step 3: Record** the `.aab` path for the Play upload (not committed).

---

### Task 17: Google Play Console — listing, testing, production — **USER ACTION (Claude drafts)**

**Files:**
- Create: `docs/store/android-listing.md` (Claude-drafted store text)

- [ ] **Step 1: Claude — draft `docs/store/android-listing.md`**: app name **ZIM Librarian**, short description (≤80 chars), full description (offline, cited answers, open models, AGPL, no data collected), category Education/Books & Reference, contact email, privacy policy URL (`https://johnsonfarmsus.github.io/zim-librarian/privacy`). Commit it.

- [ ] **Step 2: USER — recruit the 12 testers now** (starts the 14-day clock)

Create an email list (or Google Group) of ≥12 Google accounts: household members + the mutual-tester community for any remainder. Apple-ecosystem folks make a free Google account and opt in via the web link — an Android device is not strictly required to be a counted tester, though on-device testing is better.

- [ ] **Step 3: USER — create the app + upload the `.aab` to Closed testing**

Play Console → Create app (ZIM Librarian, App, Free) → **Testing → Closed testing** → create a track → upload the `.aab` from Task 16 → add the 12 testers → share the opt-in link.

- [ ] **Step 4: USER — complete the required forms**

**Data safety:** No data collected/shared. **Content rating:** complete the IARC questionnaire (reference/education content). **Target audience**, **Ads: none**, **Privacy policy URL** as above, **App access:** all functionality available without special access.

- [ ] **Step 5: USER — run the 14-day closed test**

Keep ≥12 testers opted in for 14 continuous days. Then Play Console surfaces **Apply for production access**.

- [ ] **Step 6: USER — promote to Production + submit for review**

Create a Production release, reuse the `.aab`, paste the drafted listing + `assets/screenshots/android` + `assets/play-feature-1024x500.png`, and submit.

---

## Phase 5 — Desktop signed release (parallel with Phases 3–4)

### Task 18: Release notes + tag → build installers

**Files:**
- Create: `docs/store/CHANGELOG-1.0.0.md`

- [ ] **Step 1: Write `docs/store/CHANGELOG-1.0.0.md`** — first public release: offline cited-answer librarian, bundled OLMo 2 1B on desktop, starter library, AGPL-3.0. Commit it.

- [ ] **Step 2: USER — add the Apple secrets** (so macOS builds are notarized)

Per `docs/RELEASING.md`, add repo secrets `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_CONTENT`. Without them the workflow still builds (unsigned macOS).

- [ ] **Step 3: Tag `v1.0.0` and push (triggers `release.yml`)**

```bash
cd /Users/trevorjohnson/Documents/Projects/zim-librarian
git tag v1.0.0 && git push origin main v1.0.0
```

- [ ] **Step 4: Verify the workflow produced installers**

GitHub → Actions → the `v1.0.0` run is green; a **draft** Release holds macOS `.dmg`/`.app`, Windows `.msi`/`.exe`, Linux `.AppImage`/`.deb`. If macOS is signed, on a Mac: `spctl -a -vv <app>` → "accepted / Notarized Developer ID".

- [ ] **Step 5: USER — publish the draft GitHub Release** after reviewing artifacts.

---

### Task 19: Windows signing fast-follow — SignPath Foundation — **USER ACTION (does not gate launch)**

**Files:** later `.github/workflows/release.yml` edit (post-approval).

- [ ] **Step 1: USER — apply to SignPath Foundation** (https://signpath.org) for free OV OSS code signing, referencing the public AGPL repo.
- [ ] **Step 2: On approval — Claude adds a SignPath signing step** to the Windows job in `release.yml` and re-cuts the Windows installer. Until then, Windows ships unsigned with a documented SmartScreen note in the release body.

---

## Phase 6 — Coordinated launch

### Task 20: Go-live gate

- [ ] **Step 1: Confirm all green:** repo public + Pages live (Task 6); on-device answers verified (Tasks 7–8); iOS submitted/approved (Task 14); Android in production review (Task 17); desktop GitHub Release published (Task 18).
- [ ] **Step 2: USER — release the approved iOS and Android versions** (Apple "Release this version"; Google "Roll out to production").
- [ ] **Step 3: Announce** — update README badges/links to the live store listings; commit.

---

## Self-review notes

- **Spec coverage:** A→Task 1–4,6; B→Task 5–6; C→Task 9–11; D→Task 12–14; E→Task 15–17; F→Task 18–19; verification gate→Task 7–8; coordinated launch→Task 20. All spec sections mapped.
- **USER ACTION tasks** (6, 13, 14, 17, 18-partial, 19) are account/store/credential steps Claude must not perform per the safety boundary; Claude prepares all inputs up to the click.
- **Version constant:** `1.0.0` used consistently (Global Constraints, Tasks 12 & 18).
- **Keystore safety:** gitignore rule (Task 15 Step 1) precedes generation (Step 2) and is verified (Step 4) before any commit.
