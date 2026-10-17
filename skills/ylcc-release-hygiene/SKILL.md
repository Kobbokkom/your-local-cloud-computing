---
name: ylcc-release-hygiene
description: Release and version-reference hygiene for YLCC: bump versions, align installer defaults/tests, and remove stale release references before tagging.
---

Use this skill when bumping YLCC versions, preparing a release, or fixing failed installer tests.

1. Change the package version in `Cargo.toml`, then run `cargo build` to update `Cargo.lock`.
2. Update installer defaults:
   - `scripts/install.sh`: `YLCC_VERSION` default and help text
   - `scripts/install.ps1`: `[string]$Version`
   - update-skill/install skill checks if adding skill-era references
3. Align test fixtures to the same release tag:
   - `scripts/test-install.sh` URL allow list and curl mock version
   - `scripts/test-install.ps1` `$base`, `-Version` calls
4. Search all remaining stale references:
   ```bash
   grep -R "v0\.[0-9]\+\.[0-9]\+" --include='*' . | grep -v '\.git/'
   ```
   Keep intentional historical notes; update install docs, support-branch examples, and default fallbacks.
5. Update usage-facing skill/docs if release commands changed.
6. Verify:
   ```bash
   cargo test --locked
   bash scripts/test-install.sh
   ```
   and on Windows:
   ```powershell
   powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ./scripts/test-install.ps1
   ```
7. Commit, tag, and push only after tests pass. Track the release workflow until `success`.

Rule: do not edit tests to permit an old expected failure. Update fixtures only when the tested default release tag changes.
