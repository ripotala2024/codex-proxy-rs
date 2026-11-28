#!/usr/bin/env bash
set -euo pipefail

plugin_root=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
target="${1:-$(rustc -vV | awk '/^host: / {print $2}')}"
plugin_cli="${PLUGIN_CLI:-cpr-plugin}"

case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|aarch64-apple-darwin) ;;
  *)
    echo "unsupported plugin target: $target" >&2
    exit 1
    ;;
esac

if ! command -v "$plugin_cli" >/dev/null 2>&1; then
  echo "cpr-plugin was not found. Install it from the host repository first." >&2
  exit 1
fi

cargo build --release \
  --manifest-path "$plugin_root/backend/Cargo.toml" \
  --target "$target"

"$plugin_cli" package \
  --manifest "$plugin_root/plugin.json" \
  --binary "$plugin_root/backend/target/$target/release/codex-proxy-protocol-bridge" \
  --target "$target" \
  --output-dir "$plugin_root/dist"
