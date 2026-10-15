#!/usr/bin/env python3
"""Create a locked, non-admin service account; never modifies an existing user."""
import argparse
import grp
import os
from pathlib import Path
import pwd
import re
import subprocess
import sys


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--user", default="ylcc")
    parser.add_argument("--home", required=True, type=Path)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit("Run this one-time account provisioning command as administrator")
    if not re.fullmatch(r"[a-z][a-z0-9_-]{0,30}", args.user):
        raise SystemExit("Invalid service account name")
    if not args.home.is_absolute() or ".." in args.home.parts:
        raise SystemExit("Home must be a normalized absolute path")
    for parent in [args.home, *args.home.parents]:
        if parent.is_symlink():
            raise SystemExit("Home or parent is a symlink")
    try:
        pwd.getpwnam(args.user)
    except KeyError:
        pass
    else:
        raise SystemExit("User already exists; inspect its identity rather than modifying it")
    if args.home.exists():
        raise SystemExit("Home already exists; refusing existing data")
    if sys.platform == "linux":
        run("useradd", "--create-home", "--user-group", "--shell", "/usr/sbin/nologin", "--home-dir", str(args.home), args.user)
        run("loginctl", "enable-linger", args.user)
    elif sys.platform == "darwin":
        try:
            grp.getgrnam(args.user)
        except KeyError:
            pass
        else:
            raise SystemExit("Group already exists; inspect it first")
        used = {entry.pw_uid for entry in pwd.getpwall()} | {entry.gr_gid for entry in grp.getgrall()}
        identity = next(value for value in range(501, 65536) if value not in used)
        run("dscl", ".", "-create", f"/Groups/{args.user}")
        run("dscl", ".", "-create", f"/Groups/{args.user}", "PrimaryGroupID", str(identity))
        for key, value in {"UniqueID": str(identity), "PrimaryGroupID": str(identity), "NFSHomeDirectory": str(args.home), "UserShell": "/usr/bin/false", "IsHidden": "1", "Password": "*"}.items():
            run("dscl", ".", "-create", f"/Users/{args.user}", key, value)
        args.home.mkdir(mode=0o700, parents=True)
    else:
        raise SystemExit("Only Linux and macOS are supported")
    user = pwd.getpwnam(args.user)
    os.chown(args.home, user.pw_uid, user.pw_gid)
    args.home.chmod(0o700)
    print(f"Created non-admin {args.user}; home {args.home}. No password, SSH key or sudo access installed.")


if __name__ == "__main__":
    main()
