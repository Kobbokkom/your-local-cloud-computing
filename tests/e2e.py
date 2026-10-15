#!/usr/bin/env python3
"""Real hub/worker/CLI contract test; uses only disposable local data."""
import hashlib
import json
import os
from pathlib import Path
import platform
import signal
import socket
import subprocess
import sys
import tempfile
import time


def main():
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/ylcc").resolve())
    with tempfile.TemporaryDirectory(prefix="ylcc-e2e-") as temporary:
        base = Path(temporary)
        token = base / "token"
        subprocess.run([binary, "--token-file", str(token), "init"], check=True, capture_output=True)
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        hub_url = f"http://127.0.0.1:{port}"
        prefix = [binary, "--hub", hub_url, "--token-file", str(token), "--results-dir", str(base / "results"), "--json"]
        project = base / "project"
        project.mkdir()
        subprocess.run(["git", "init", "-q", str(project)], check=True)
        (project / ".gitignore").write_text("ignored.txt\napp\nanswer.txt\n")
        (project / "input.txt").write_text("41\n")
        (project / "ignored.txt").write_text("explicit only\n")
        (project / "main.c").write_text('#include <stdio.h>\nint main(void){int n;if(scanf("%d",&n)!=1)return 2;printf("%d\\n",n+1);return 0;}\n')
        subprocess.run(["git", "-C", str(project), "add", "input.txt", "main.c", ".gitignore"], check=True)
        (project / "input.txt").write_text("42\n")
        processes = []
        streams = []

        def start(role, *args):
            stream = (base / f"{role}-{len(streams)}.log").open("w+")
            streams.append(stream)
            process = subprocess.Popen(prefix + [role, *args], stdout=stream, stderr=stream, start_new_session=True)
            processes.append(process)
            return process

        def cli(*args, ok=True, timeout=90):
            result = subprocess.run(prefix + list(args), cwd=project, text=True, capture_output=True, timeout=timeout)
            if ok and result.returncode:
                raise AssertionError(f"{args}: {result.stderr}\n{result.stdout}")
            if not ok:
                assert result.returncode != 0, result.stdout
                return result
            return json.loads(result.stdout), result.stderr

        def until(function, timeout=60):
            deadline = time.monotonic() + timeout
            last = None
            while time.monotonic() < deadline:
                try:
                    last = function()
                    if last:
                        return last
                except (AssertionError, json.JSONDecodeError):
                    pass
                time.sleep(0.2)
            raise AssertionError(f"timed out waiting; last={last}")

        def state(job):
            return cli("jobs", "show", job["id"])[0]

        def wait(job):
            return cli("jobs", "wait", job["id"])[0]

        def result(job, name="answer.txt"):
            return (base / "results" / job["id"] / name).read_text()

        def stop(process):
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()

        try:
            hub = start("hub", "--listen", f"127.0.0.1:{port}", "--data-dir", str(base / "hub"))
            until(lambda: cli("workers")[0] == [])
            worker = start("worker", "--id", "test-worker", "--data-dir", str(base / "worker"))
            until(lambda: any(w["online"] for w in cli("workers")[0]))
            os_name = "macos" if sys.platform == "darwin" else "linux"
            arch = "aarch64" if platform.machine() in ("arm64", "aarch64") else "x86_64"
            common = ["--os", os_name, "--arch", arch]

            empty, _ = cli("run", "--no-input", *common, "--", "true")
            assert empty["state"] == "succeeded" and not empty["spec"]["inputs"]

            manifest, _ = cli("manifest", "--project")
            assert "ignored.txt" not in {f["path"] for f in manifest["files"]}
            assert all(not f["path"].startswith(".git/") for f in manifest["files"])
            explicit, _ = cli("manifest", "--input", "ignored.txt")
            assert [f["path"] for f in explicit["files"]] == ["ignored.txt"]

            first, _ = cli("run", "--project", "--output", "answer.txt", *common, "--tool", "cc=*", "--", "sh", "-c", "cc main.c -o app && ./app < input.txt > answer.txt")
            assert first["state"] == "succeeded" and result(first) == "43\n"
            assert first["worker_id"] == "test-worker"
            assert not (project / "answer.txt").exists()
            assert hashlib.sha256(result(first).encode()).hexdigest() == first["results"][0]["hash"]
            second, transfer = cli("run", "--project", "--output", "answer.txt", *common, "--", "sh", "-c", "cat input.txt > answer.txt")
            assert "uploaded 0 missing blobs (0 bytes)" in transfer

            frozen, _ = cli("run", "--input", "input.txt", "--output", "answer.txt", *common, "--detach", "--", "sh", "-c", "sleep 2; cat input.txt > answer.txt")
            (project / "input.txt").write_text("99\n")
            assert result(wait(frozen)) == "42\n"
            changed, transfer = cli("run", "--input", "input.txt", "--output", "answer.txt", *common, "--", "sh", "-c", "test ! -e main.c; cat input.txt > answer.txt")
            assert result(changed) == "99\n" and "uploaded 1 missing blobs" in transfer
            rejected = cli("run", "--input", "input.txt", "--arch", "impossible-cpu", "--", "true", ok=False)
            assert "no compatible" in rejected.stderr

            busy, _ = cli("run", "--input", "input.txt", *common, "--detach", "--", "sleep", "3")
            queued, _ = cli("run", "--input", "input.txt", *common, "--detach", "--", "true")
            assert queued["state"] == "queued"
            wait(busy)
            wait(queued)

            cancel, _ = cli("run", "--input", "input.txt", *common, "--detach", "--", "sh", "-c", "sleep 60 & wait")
            until(lambda: state(cancel)["state"] == "running")
            cli("jobs", "cancel", cancel["id"])
            until(lambda: state(cancel)["state"] == "cancelled", timeout=15)

            reconnect, _ = cli("run", "--input", "input.txt", "--output", "answer.txt", *common, "--detach", "--", "sh", "-c", "printf start; sleep 5; printf done > answer.txt")
            until(lambda: state(reconnect)["state"] == "running")
            stop(hub)
            time.sleep(1)
            hub = start("hub", "--listen", f"127.0.0.1:{port}", "--data-dir", str(base / "hub"))
            until(lambda: any(w["online"] for w in cli("workers")[0]))
            resumed = wait(reconnect)
            assert resumed["attempt_id"] == reconnect["attempt_id"] and result(resumed) == "done"
            logs, _ = cli("jobs", "logs", reconnect["id"])
            assert len({line["seq"] for line in logs}) == len(logs)
            assert "".join(line["text"] for line in logs).count("start") == 1

            environment = {"id": "test-tools", "revision": "v1", "requirements": {"os": os_name, "arch": arch}, "setup": ['mkdir -p "$YLCC_ENV_DIR/bin"; printf \'#!/bin/sh\\nprintf prepared\\n\' > "$YLCC_ENV_DIR/bin/prepared"; chmod +x "$YLCC_ENV_DIR/bin/prepared"'], "verify": ['test "$(prepared)" = prepared']}
            environment_file = base / "environment.json"
            environment_file.write_text(json.dumps(environment))
            cli("env", "add", str(environment_file))
            prepared, _ = cli("run", "--input", "input.txt", "--output", "answer.txt", *common, "--environment", "test-tools", "--", "sh", "-c", "prepared > answer.txt")
            assert result(prepared) == "prepared"
            statuses, _ = cli("env", "status")
            assert any(s["state"] == "ready" and s["revision"] == "v1" for s in statuses)
            stop(worker)
            worker = start("worker", "--id", "test-worker", "--data-dir", str(base / "worker"))
            until(lambda: any(w["online"] for w in cli("workers")[0]))
            reused, _ = cli("run", "--input", "input.txt", "--output", "answer.txt", *common, "--environment", "test-tools", "--", "sh", "-c", "prepared > answer.txt")
            assert result(reused) == "prepared"
            print("PASS: native compile, dirty/ignored snapshot, dedup, isolation, queue, cancellation, reconnect, persistent environment")
        except BaseException:
            for stream in streams:
                stream.flush()
                print(f"--- {stream.name} ---", file=sys.stderr)
                print(Path(stream.name).read_text()[-12000:], file=sys.stderr)
            raise
        finally:
            for process in reversed(processes):
                stop(process)
            for stream in streams:
                stream.close()


if __name__ == "__main__":
    main()
