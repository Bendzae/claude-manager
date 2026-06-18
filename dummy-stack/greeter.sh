#!/usr/bin/env bash
# Reads dummy-stack/config.json and prints a greeting.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
greeting=$(grep -o '"greeting": *"[^"]*"' "$DIR/config.json" | sed 's/.*"greeting": *"\([^"]*\)".*/\1/')
name=$(grep -o '"name": *"[^"]*"' "$DIR/config.json" | sed 's/.*"name": *"\([^"]*\)".*/\1/')
echo "$greeting, $name!"
