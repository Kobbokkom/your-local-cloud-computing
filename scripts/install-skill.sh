#!/usr/bin/env bash
set -euo pipefail
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../skills/ylcc" && pwd)"
destination="${HOME:?HOME is required}/.agents/skills/ylcc"
case "${1:---global}" in
  --global) [[ $# -le 1 ]] || { printf 'Usage: install-skill.sh [--global | --project DIR]\n' >&2; exit 1; } ;;
  --project) [[ $# == 2 ]] || { printf 'Usage: install-skill.sh --project DIR\n' >&2; exit 1; }; destination="${2%/}/.agents/skills/ylcc" ;;
  -h|--help) printf 'Usage: install-skill.sh [--global | --project DIR]\n'; exit 0 ;;
  *) printf 'Usage: install-skill.sh [--global | --project DIR]\n' >&2; exit 1 ;;
esac
if [[ -e "$destination" || -L "$destination" ]]; then
  if [[ ! -L "$destination" && -f "$destination/SKILL.md" ]] && cmp -s "$source_dir/SKILL.md" "$destination/SKILL.md"; then
    printf 'YLCC skill already installed at %s\n' "$destination"
    exit 0
  fi
  printf 'Refusing to overwrite existing skill at %s. Review and remove it explicitly before updating.\n' "$destination" >&2
  exit 1
fi
mkdir -p "$destination"
cp "$source_dir/SKILL.md" "$destination/SKILL.md"
printf 'Installed YLCC skill at %s. Start a new agent session to discover it.\n' "$destination"
