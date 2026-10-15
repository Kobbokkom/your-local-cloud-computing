#!/usr/bin/env python3
"""Collect legal texts for the optional Docker CLI; build-time stdlib only."""
import argparse
import io
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import urllib.request


def versions(client):
    docker, go = client["Version"], client["GoVersion"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", docker):
        raise ValueError("Expected a stable Docker CLI version")
    if not re.fullmatch(r"go[0-9]+\.[0-9]+\.[0-9]+", go):
        raise ValueError("Expected a stable Docker CLI Go version")
    return docker, go


def collect(source, required):
    files = {}
    with tarfile.open(fileobj=source, mode="r|gz") as archive:
        for member in archive:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("Unsafe source archive path")
            if member.isdir():
                continue
            if not re.match(r"^(?:licen[cs]e|copying|copyright|notice|authors|patents)(?:$|[._-])", path.name, re.I):
                continue
            if path.suffix.lower() in (".go", ".c", ".h", ".py", ".sh", ".rs"):
                continue
            if not member.isfile() or member.size > 16 * 1024 * 1024:
                raise ValueError(f"Legal text must be a bounded regular file: {member.name}")
            relative = PurePosixPath(*path.parts[1:]).as_posix()
            if relative in files:
                raise ValueError("Duplicate legal text in source archive")
            data = archive.extractfile(member).read()
            try:
                files[relative] = data.decode("utf-8")
            except UnicodeDecodeError:
                files[relative] = data.decode("latin-1")
    if not set(required) <= files.keys():
        raise ValueError("Source archive is missing required upstream legal texts")
    return "".join(f"\n--- {name} ---\n{files[name].rstrip()}\n" for name in sorted(files))


def self_test():
    def fixture(kind=tarfile.REGTYPE, name="root/LICENSE"):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            member = tarfile.TarInfo(name)
            member.type, member.size, member.linkname = kind, 7 if kind == tarfile.REGTYPE else 0, "other"
            archive.addfile(member, io.BytesIO(b"notice\n") if member.isfile() else None)
        output.seek(0)
        return output
    assert "notice" in collect(fixture(), ["LICENSE"])
    for source in (fixture(tarfile.SYMTYPE), fixture(tarfile.LNKTYPE), fixture(name="../LICENSE")):
        try:
            collect(source, ["LICENSE"])
        except ValueError:
            continue
        raise AssertionError("Unsafe archive was accepted")
    assert versions({"Version": "28.5.2", "GoVersion": "go1.25.3"}) == ("28.5.2", "go1.25.3")
    print("Docker CLI notice fixtures passed.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("DOCKER-CLI-NOTICES.txt"))
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    # `docker version` emits Client JSON but exits nonzero without a daemon.
    result = subprocess.run(["docker", "version", "--format", "{{json .Client}}"],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    docker, go = versions(json.loads(result.stdout))
    sources = [(f"https://github.com/docker/cli/archive/refs/tags/v{docker}.tar.gz", ["LICENSE", "NOTICE"]),
               (f"https://go.dev/dl/{go}.src.tar.gz", ["LICENSE"])]
    sections = [f"Docker CLI {docker}; compiled with {go}\nUpstream legal texts, including vendored dependencies.\n"]
    for url, required in sources:
        with urllib.request.urlopen(url, timeout=120) as source:
            if not source.geturl().startswith("https://"):
                raise ValueError("Source downloads must use HTTPS")
            sections.append(f"\n===== {url} =====\n" + collect(source, required))
    args.output.write_text("".join(sections), encoding="utf-8")
    print(f"Generated Docker CLI {docker} / {go} notices.")


if __name__ == "__main__":
    main()
