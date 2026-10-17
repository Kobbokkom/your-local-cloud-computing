#!/usr/bin/env bash
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../skills/ylcc" && pwd)"
destination="${HOME:?HOME is required}/.agents/skills/ylcc"
case "${1:---global}" in
  --global) [[ $# -le 1 ]] || { printf 'Usage: install-skill.sh [--global | --project DIR]\n' >&2; exit 1; } ;;
  --project) [[ $# == 2 ]] || { printf 'Usage: install-skill.sh [--global | --project DIR]\n' >&2; exit 1; }; destination="${2%/}/.agents/skills/ylcc" ;;
  -h|--help) printf 'Usage: install-skill.sh [--global | --project DIR]\n'; exit 0 ;;
  *) printf 'Usage: install-skill.sh [--global | --project DIR]\n' >&2; exit 1 ;;
esac
if [[ -L "$destination" ]]; then
  printf 'Refusing to overwrite existing skill at %s. Review and remove it explicitly before updating.\n' "$destination" >&2
  exit 1
fi
if [[ -e "$destination" ]]; then
  if [[ ! -f "$destination/SKILL.md" ]] || ! cmp -s "$source_dir/SKILL.md" "$destination/SKILL.md"; then
    printf 'Refusing to overwrite existing skill at %s. Review and remove it explicitly before updating.\n' "$destination" >&2
    exit 1
  fi
  # Keep local customizations while adding compatible companion files.
  for source_file in "$source_dir"/*; do
    [[ -f "$source_file" ]] || continue
    target_file="$destination/$(basename "$source_file")"
    [[ -e "$target_file" ]] || cp "$source_file" "$target_file"
  done
  printf 'YLCC skill already installed at %s\n' "$destination"
  exit 0
fi
mkdir -p "$destination"
cp -R "$source_dir/." "$destination/"
printf 'Installed YLCC skill at %s. Start a new agent session to discover it.\n' "$destination"
