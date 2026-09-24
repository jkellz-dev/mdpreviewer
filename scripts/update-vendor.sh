#!/usr/bin/env sh
# Re-download the vendored browser assets (mermaid.js and the GitHub markdown
# theme). Versions can be overridden via environment variables.
set -eu

MERMAID_VERSION="${MERMAID_VERSION:-11.4.1}"
GH_MD_CSS_VERSION="${GH_MD_CSS_VERSION:-5.8.1}"

root="$(cd "$(dirname "$0")/.." && pwd)"
vendor="$root/assets/vendor"
mkdir -p "$vendor"

printf 'Fetching mermaid@%s...\n' "$MERMAID_VERSION"
curl -fsSL \
  "https://cdn.jsdelivr.net/npm/mermaid@${MERMAID_VERSION}/dist/mermaid.min.js" \
  -o "$vendor/mermaid.min.js"

printf 'Fetching github-markdown-css@%s (dark)...\n' "$GH_MD_CSS_VERSION"
curl -fsSL \
  "https://cdn.jsdelivr.net/npm/github-markdown-css@${GH_MD_CSS_VERSION}/github-markdown-dark.css" \
  -o "$vendor/github-markdown.css"

printf 'Done. Updated vendored assets in %s\n' "$vendor"
