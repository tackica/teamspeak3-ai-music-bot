#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
hooks_dir="$repo_root/.git/hooks"
source_dir="$repo_root/githooks"

if [ ! -d "$hooks_dir" ]; then
  echo "Could not find .git/hooks at $hooks_dir" >&2
  exit 1
fi

for hook in pre-commit pre-push; do
  if [ ! -f "$source_dir/$hook" ]; then
    echo "Missing hook template: $source_dir/$hook" >&2
    exit 1
  fi

  cp "$source_dir/$hook" "$hooks_dir/$hook"
  chmod 0755 "$hooks_dir/$hook"
  echo "Installed $hooks_dir/$hook"
done

echo "Git hooks are now active."
