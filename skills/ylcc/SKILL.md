---
name: ylcc
description: Offload builds, tests, rendering, GPU computation, and other expensive commands to the user's YLCC worker pool, or run work that requires a different operating system or CPU architecture.
---

Use the installed `ylcc` CLI to execute work on the configured private pool. Read `ylcc --help` and the relevant subcommand help when an option is uncertain. The CLI writes JSON to stdout and progress to stderr; `--json` makes machine-readable intent explicit. Hub/token defaults come from `ylcc configure` or `YLCC_HUB_URL` / `YLCC_TOKEN_FILE`. Never print token contents or include them in commands, source snapshots, or outputs.

1. Run `ylcc --json workers` and inspect each worker's actual capabilities. Choose the execution OS, architecture, toolchain and GPU backend the task requires. Distinguish execution platform from the produced binary's build target. A Docker worker on a Mac executes Linux; CUDA and Metal are different backends. Do not infer that requesting a GPU rewrites a CPU program to use it.
2. Select the necessary source files and folders with repeated `--input`. Use `ylcc --json manifest --input PATH` to inspect included files and exclusion reasons. Folder/project selection honors Git ignore rules, with tracked working-tree content included. An explicitly selected ignored file is included; check it contains no unintended secrets. Use `--project` only when the whole project is needed; use `--no-input` when no local files are needed. Inputs are fixed at submission, so submit again after changing source.
3. Reuse an appropriate registered environment: `ylcc --json env list` and `ylcc --json env status`. A versioned environment can prepare its user-space tools automatically when the job first uses it. To prepare explicitly, use `ylcc env prepare ENVIRONMENT_ID --worker WORKER_ID`. If setup fails, read the error, correct the recipe or requirements, and prepare again. Host drivers, Xcode, credentials, and administrative changes need separate setup; installed CLI presence does not prove API authentication.
4. Submit with explicit outputs and platform requirements. Example:

   ```bash
   ylcc --json run --input Cargo.toml --input Cargo.lock --input src \
     --os linux --arch x86_64 --environment rust-linux \
     --output target/release/my-tool --timeout 1800 --detach \
     -- cargo build --locked --release
   ```

   Use the project's real command and output paths. The returned `.id` identifies the job. Busy workers queue work; there is no automatic local fallback. Continue independent work while it runs.
5. Use `ylcc --json jobs show JOB_ID` for status and `ylcc --json jobs logs JOB_ID` for logs. `ylcc --json jobs wait JOB_ID` waits for completion and automatically fetches declared outputs into the configured results directory under `JOB_ID`. `jobs fetch JOB_ID` explicitly fetches or reports that directory. `--no-download` skips automatic downloads. Apply needed files to the project deliberately; the CLI does not overwrite project sources.
6. Report the worker/platform, exit status, verification performed, and result path. If a worker disconnects or an attempt becomes `unknown`, inspect the original attempt before resubmitting; a missing connection does not prove the command stopped. Use `ylcc jobs cancel JOB_ID` to cancel. Do not silently duplicate work or run a heavy fallback on the requester.
