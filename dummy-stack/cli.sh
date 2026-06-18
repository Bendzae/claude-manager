#!/usr/bin/env bash
# Thin CLI wrapper around greeter.sh. Supports --upper to shout the greeting.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$("$DIR/greeter.sh")"
if [[ "${1:-}" == "--upper" ]]; then
  out="$(echo "$out" | tr '[:lower:]' '[:upper:]')"
fi
echo "$out"
