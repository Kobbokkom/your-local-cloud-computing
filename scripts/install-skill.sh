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
if [[ -e "$destination" ]]; then
  # Remove the previous skill tree only. This directory contains only public skill files.
  rm -rf "$destination"
fi
mkdir -p "$destination"
cp -R "$source_dir/." "$destination/"
printf 'Installed YLCC skill at %s. Start a new agent session to discover it.\n' "$destination"
