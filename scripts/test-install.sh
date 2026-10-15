#!/usr/bin/env bash
# Exercise actual installation, corruption refusal, and skill overwrite protection without network access.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
task_tmp="$(mktemp -d)"
trap 'rm -rf "$task_tmp"' EXIT
mkdir -p "$task_tmp/mock" "$task_tmp/release" "$task_tmp/payload" "$task_tmp/user"
printf '#!/bin/sh\nprintf "ylcc test\\n"\n' > "$task_tmp/payload/ylcc"
chmod +x "$task_tmp/payload/ylcc"
cat > "$task_tmp/mock/uname" <<'MOCK'
#!/bin/sh
case "$1" in -s) printf '%s\n' "$TEST_OS" ;; -m) printf '%s\n' "$TEST_ARCH" ;; *) exit 1 ;; esac
MOCK
cat > "$task_tmp/mock/curl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
url='' destination=''
while (($#)); do
  case "$1" in
    -o) destination="$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    *) shift ;;
  esac
done
cp "$TEST_RELEASE/${url##*/}" "$destination"
MOCK
chmod +x "$task_tmp/mock/"*
export PATH="$task_tmp/mock:$PATH" TEST_RELEASE="$task_tmp/release"
for platform in 'Linux x86_64 linux x86_64' 'Linux aarch64 linux aarch64' 'Darwin x86_64 macos x86_64' 'Darwin arm64 macos aarch64'; do
  read -r TEST_OS TEST_ARCH os arch <<< "$platform"
  export TEST_OS TEST_ARCH
  archive="ylcc-$os-$arch.tar.gz"
  tar -czf "$TEST_RELEASE/$archive" -C "$task_tmp/payload" ylcc
  if command -v sha256sum >/dev/null; then hash="$(sha256sum "$TEST_RELEASE/$archive")"; else hash="$(shasum -a 256 "$TEST_RELEASE/$archive")"; fi
  printf '%s  %s\n' "${hash%% *}" "$archive" > "$TEST_RELEASE/SHA256SUMS"
  bash "$repo/scripts/install.sh" --prefix "$task_tmp/prefix with spaces" >/dev/null
  [[ "$("$task_tmp/prefix with spaces/bin/ylcc")" == 'ylcc test' ]]
done
printf '\ncorrupt\n' >> "$TEST_RELEASE/$archive"
if bash "$repo/scripts/install.sh" --prefix "$task_tmp/rejected" >/dev/null 2>&1; then
  printf 'Corrupt download was accepted\n' >&2; exit 1
fi
[[ ! -e "$task_tmp/rejected/bin/ylcc" ]]
HOME="$task_tmp/user" bash "$repo/scripts/install-skill.sh" >/dev/null
HOME="$task_tmp/user" bash "$repo/scripts/install-skill.sh" >/dev/null
printf '\nlocal customization\n' >> "$task_tmp/user/.agents/skills/ylcc/SKILL.md"
if HOME="$task_tmp/user" bash "$repo/scripts/install-skill.sh" >/dev/null 2>&1; then
  printf 'Existing customized skill was overwritten\n' >&2; exit 1
fi
bash "$repo/scripts/install-skill.sh" --project "$task_tmp/project with spaces" >/dev/null
cmp "$repo/skills/ylcc/SKILL.md" "$task_tmp/project with spaces/.agents/skills/ylcc/SKILL.md"
printf 'Installer checks passed: four platforms, spaces, corrupt download, skill idempotence and overwrite protection.\n'
