#!/bin/bash
# Migration script: disable auto_context for all existing tasks and remove old hooks from worktrees.
# Run this once after upgrading to the new task context system.

set -euo pipefail

CONFIG_FILE="$HOME/.claude-manager/config.toml"
WORKTREES_DIR="$HOME/.claude-manager/worktrees"

echo "=== Claude Manager: auto_context migration ==="
echo ""

# 1. Flip auto_context to false in config.toml
if [ -f "$CONFIG_FILE" ]; then
    # Replace auto_context = true with auto_context = false
    if grep -q 'auto_context = true' "$CONFIG_FILE"; then
        sed -i.bak 's/auto_context = true/auto_context = false/g' "$CONFIG_FILE"
        echo "[OK] Flipped all auto_context = true -> false in $CONFIG_FILE"
        echo "     Backup saved as ${CONFIG_FILE}.bak"
    else
        echo "[OK] No auto_context = true found in $CONFIG_FILE (already migrated or never set)"
    fi
else
    echo "[SKIP] Config file not found at $CONFIG_FILE"
fi

echo ""

# 2. Remove old hooks from worktree .claude/settings.local.json files
count=0
if [ -d "$WORKTREES_DIR" ]; then
    while IFS= read -r settings_file; do
        # Remove the hooks key from the JSON
        if command -v jq &> /dev/null; then
            tmp=$(mktemp)
            jq 'del(.hooks)' "$settings_file" > "$tmp" && mv "$tmp" "$settings_file"
            echo "[OK] Removed hooks from $settings_file"
            ((count++)) || true
        else
            echo "[WARN] jq not found — cannot auto-remove hooks from $settings_file"
            echo "       Manually remove the 'hooks' key from this file."
        fi
    done < <(find "$WORKTREES_DIR" -path '*/.claude/settings.local.json' -type f 2>/dev/null)
fi

if [ "$count" -eq 0 ]; then
    echo "[OK] No worktree settings files found to clean up"
else
    echo ""
    echo "[OK] Cleaned hooks from $count worktree settings file(s)"
fi

echo ""
echo "Migration complete. New sessions will use the base system prompt."
echo "Toggle auto_context back on per-task if you want async context updates."
