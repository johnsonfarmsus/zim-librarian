#!/bin/sh
# Fetch the default model bundled into desktop installers (OLMo 2 1B
# Instruct, Apache-2.0). Run before `cargo tauri build`; CI release builds
# run it automatically. Skips the download when the file is already there.
set -e
cd "$(dirname "$0")/../crates/app-tauri"
mkdir -p resources
FILE="resources/OLMo-2-0425-1B-Instruct-Q4_K_M.gguf"
URL="https://huggingface.co/allenai/OLMo-2-0425-1B-Instruct-GGUF/resolve/main/OLMo-2-0425-1B-Instruct-Q4_K_M.gguf"
BYTES=935515296

have=$(wc -c < "$FILE" 2>/dev/null || echo 0)
if [ "$have" -eq "$BYTES" ]; then
  echo "bundled model already present: $FILE"
  exit 0
fi
echo "downloading OLMo 2 1B (~0.9 GB) -> $FILE"
curl -L --fail --progress-bar -o "$FILE.part" "$URL"
got=$(wc -c < "$FILE.part")
if [ "$got" -ne "$BYTES" ]; then
  echo "size mismatch: got $got, expected $BYTES" >&2
  rm -f "$FILE.part"
  exit 1
fi
mv "$FILE.part" "$FILE"
echo "done"
