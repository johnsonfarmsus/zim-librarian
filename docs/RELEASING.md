# Releasing ZIM Librarian

## Cutting a release

1. Bump the version in **three** places (they must match):
   - `Cargo.toml` (workspace root)
   - `crates/app-tauri/Cargo.toml`
   - `crates/app-tauri/tauri.conf.json`
2. Commit, then tag and push:
   ```sh
   git tag v0.2.0 && git push origin main v0.2.0
   ```
3. The `Release desktop apps` workflow builds macOS/Windows/Linux
   installers (each bundling the OLMo 2 1B model, ~1 GB) and attaches them
   to a **draft** GitHub release. Review and publish.

## macOS signing + notarization (one-time setup)

Warning-free installs on macOS need a Developer ID signature and Apple
notarization. With an Apple Developer account:

1. **Developer ID Application certificate**
   - developer.apple.com → Certificates → `+` → *Developer ID Application*.
     Create the CSR with Keychain Access (Certificate Assistant → Request a
     Certificate) on this Mac, upload it, download and double-click the cert.
   - Export it from Keychain Access as a `.p12` with a strong password.
   - Base64 it for GitHub: `base64 -i DeveloperID.p12 | pbcopy`
2. **App Store Connect API key** (for notarization)
   - appstoreconnect.apple.com → Users and Access → Integrations →
     App Store Connect API → `+`. Role: *Developer* is enough.
   - Note the **Key ID** and **Issuer ID**; download the `.p8` once.
3. **GitHub repo secrets** (Settings → Secrets and variables → Actions):

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | base64 of the `.p12` |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
   | `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Trevor Johnson (TEAMID)` |
   | `APPLE_API_KEY` | the API **Key ID** |
   | `APPLE_API_ISSUER` | the **Issuer ID** |
   | `APPLE_API_KEY_CONTENT` | the full text of the `.p8` file |

Without these secrets the workflow still runs and produces unsigned
artifacts (Gatekeeper warning on first open: right-click → Open).

Windows is currently unsigned — SmartScreen shows a warning; an OV/EV
Authenticode certificate can be added to the workflow later the same way.

## Mobile

- **iOS**: `cd crates/app-tauri && cargo tauri ios build` (needs Xcode and
  the Apple Developer account signed in). Distribute via TestFlight /
  App Store Connect. Models and books are downloaded in-app on first run —
  nothing large ships in the .ipa.
- **Android**: set `ANDROID_HOME`, `NDK_HOME`, `ANDROID_NDK`, and
  `JAVA_HOME`, then `cargo tauri android build --apk --target aarch64`.
  For 32-bit devices add `--target armv7` with
  `CARGO_CFG_TARGET_FEATURE="neon,vfp2,vfp3"` exported (works around the
  llama-cpp-sys build script's missing-feature panic). Play Store uploads
  need an upload keystore configured in
  `crates/app-tauri/gen/android/app/build.gradle.kts` (the release build
  is currently debug-signed for local testing only).

## Store notes

- Post-install downloads of models/ZIMs are standard practice for local-LLM
  apps (PocketPal AI, LLMFarm precedents) — allowed on both stores.
- 32-bit Android devices (armeabi-v7a) cannot memory-map books larger than
  their address space allows; the starter-library books (0.5–2.2 GB) are
  effectively 64-bit-only. Keep small books (< ~300 MB) for 32-bit users.
- The Mac App Store is deliberately not targeted for now: its mandatory
  sandbox conflicts with reference-in-place ZIM files anywhere on disk.
  Notarized direct download is the macOS channel.
