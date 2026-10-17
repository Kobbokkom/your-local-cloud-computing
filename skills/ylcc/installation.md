# Install YLCC

This reference is for agents already using the YLCC skill but finds that the CLI or support files are missing.

1. Download the release binary for the current OS/architecture and verify it against `SHA256SUMS`.
2. Put the executable on PATH, then run `ylcc --version`.
3. Clone a matching support checkout for examples, skills, and environment recipes:
   `git clone --depth 1 --branch <version> https://github.com/semanticist21/your-local-cloud-computing.git ~/.local/share/ylcc/support/<version>`
4. Install or update the skill with `YLCC_SUPPORT_DIR/./scripts/install-skill.sh`.
5. Configure without printing token values:
   - `ylcc --json doctor`
   - `ylcc setup`
   - `ylcc init`
   - transfer the protected token over a secure side channel to every machine that should use the same pool
   - `ylcc configure --hub <private HTTPS URL> --token-file <token path> --results-dir <results dir>`
6. Start or inspect workers:
   - worker data goes under the service user's owned directory
   - token mode must be 600, which YLCC creates at `init`
7. Validate:
   - `ylcc --json status`
   - `ylcc --json workers`
   - `ylcc --json examples`
   - `ylcc task init --runtime <runtime> --name smoke`
   - `ylcc task run smoke --detach`

The skill does not install, transfer secrets, or start services automatically. If a user asks to set up YLCC, ask for the Hub URL and the secure token-transfer method instead of asking for secret text.
