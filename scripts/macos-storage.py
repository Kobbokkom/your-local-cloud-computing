#!/usr/bin/env python3
"""Manage a dedicated YLCC APFS image without changing its parent volume."""
import argparse
import os
from pathlib import Path
import plistlib
import re
import stat
import subprocess
import sys
import tempfile


LABEL = "org.ylcc.storage"
HELPER = Path("/usr/local/libexec/ylcc/macos-storage.py")
SERVICE = Path("/Library/LaunchDaemons/org.ylcc.storage.plist")


def checked_path(value):
    if not value.startswith("/") or value == "/" or os.path.normpath(value) != value or any(c in value for c in "\0\n\r"):
        raise ValueError("Paths must be normalized absolute paths other than /")
    path = Path(value)
    for parent in [path, *path.parents]:
        if parent.is_symlink():
            raise ValueError("Paths and their parents must not be symlinks")
    return path


def run(*args, plist=False):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors="replace").strip() or f"{args[0]} failed")
    return plistlib.loads(result.stdout) if plist else result.stdout


def mounted_image(info, image, mountpoint):
    found = False
    for item in info.get("images", []):
        source = item.get("image-path")
        points = [entry["mount-point"] for entry in item.get("system-entities", []) if "mount-point" in entry]
        if str(mountpoint) in points:
            if source != str(image):
                raise ValueError("Mountpoint belongs to a different disk image")
            found = True
        elif source == str(image) and points:
            raise ValueError("Disk image is already mounted elsewhere")
    return found


def create(image, size):
    if image.suffix != ".sparsebundle" or not re.fullmatch(r"[1-9][0-9]*[kmgtKMGT]?", size):
        raise ValueError("Use a .sparsebundle path and a size such as 64g")
    if image.exists() or not image.parent.is_dir():
        raise ValueError("Image must not exist and its parent must already exist")
    run("/usr/bin/hdiutil", "create", "-size", size, "-type", "SPARSEBUNDLE", "-fs", "APFS", "-volname", "YLCC", str(image))
    if not image.is_dir() or image.is_symlink():
        raise RuntimeError("hdiutil did not create the requested sparsebundle")


def mount(image, mountpoint):
    if not image.is_dir() or image.is_symlink():
        raise ValueError("Disk image is absent; the external volume may not be mounted")
    info = run("/usr/bin/hdiutil", "info", "-plist", plist=True)
    if not mounted_image(info, image, mountpoint):
        if os.path.ismount(mountpoint) or (mountpoint.exists() and (not mountpoint.is_dir() or any(mountpoint.iterdir()))):
            raise ValueError("Refusing an occupied mountpoint")
        if not mountpoint.parent.is_dir():
            raise ValueError("Mountpoint parent is absent")
        run("/usr/bin/hdiutil", "attach", "-nobrowse", "-owners", "on", "-mountpoint", str(mountpoint), str(image))
        if not mounted_image(run("/usr/bin/hdiutil", "info", "-plist", plist=True), image, mountpoint):
            raise RuntimeError("Mounted filesystem does not map to the requested image")
    volume = run("/usr/sbin/diskutil", "info", "-plist", str(mountpoint), plist=True)
    if volume.get("GlobalPermissionsEnabled") is not True:
        raise RuntimeError("Mounted image does not enforce file ownership")


def service_definition(image, mountpoint):
    return {
        "Label": LABEL,
        "ProgramArguments": ["/usr/bin/python3", str(HELPER), "mount", "--image", str(image), "--mountpoint", str(mountpoint)],
        "RunAtLoad": True,
        "KeepAlive": {"SuccessfulExit": False},
        "ThrottleInterval": 30,
        "ProcessType": "Background",
        "Umask": 0o077,
    }


def protected_file(path):
    checked_path(str(path))
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise ValueError("Service files must be regular, root-owned and not group/world writable")


def install_file(path, data, mode):
    checked_path(str(path))
    if path.exists():
        protected_file(path)
        if path.read_bytes() != data:
            raise ValueError("Existing service file differs; uninstall it before replacing it")
        return
    for parent in reversed([path.parent, *path.parent.parents]):
        if not parent.exists():
            parent.mkdir(mode=0o755)
            parent.chmod(0o755)
        metadata = parent.stat()
        if metadata.st_uid != 0 or metadata.st_mode & 0o022:
            raise ValueError("Service file parents must be root-owned and not group/world writable")
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            os.fchmod(output.fileno(), mode)
        os.link(temporary, path)
    finally:
        os.unlink(temporary)


def loaded():
    return subprocess.run(["/bin/launchctl", "print", f"system/{LABEL}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


def install_service(image, mountpoint):
    definition = service_definition(image, mountpoint)
    if loaded() and not SERVICE.exists():
        raise ValueError("Storage service is loaded without its expected definition")
    if SERVICE.exists():
        protected_file(SERVICE)
        if plistlib.loads(SERVICE.read_bytes()) != definition:
            raise ValueError("Existing storage service has different settings")
    install_file(HELPER, Path(__file__).read_bytes(), 0o755)
    install_file(SERVICE, plistlib.dumps(definition), 0o644)
    if not loaded():
        run("/bin/launchctl", "enable", f"system/{LABEL}")
        run("/bin/launchctl", "bootstrap", "system", str(SERVICE))


def uninstall_service(image, mountpoint):
    if not SERVICE.exists():
        if loaded():
            raise ValueError("Storage service is loaded without its expected definition")
        return
    protected_file(SERVICE)
    if plistlib.loads(SERVICE.read_bytes()) != service_definition(image, mountpoint):
        raise ValueError("Refusing to remove a different storage service")
    if HELPER.exists():
        protected_file(HELPER)
        if HELPER.read_bytes() != Path(__file__).read_bytes():
            raise ValueError("Installed helper differs from the executing helper")
    if loaded():
        run("/bin/launchctl", "bootout", f"system/{LABEL}")
    SERVICE.unlink()
    if HELPER.exists():
        HELPER.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["init", "create", "mount", "install-service", "uninstall-service"])
    parser.add_argument("--image", required=True)
    parser.add_argument("--mountpoint", required=True)
    parser.add_argument("--size", default="64g")
    args = parser.parse_args()
    if sys.platform != "darwin" or os.geteuid() != 0:
        raise ValueError("Run on macOS as administrator")
    os.umask(0o077)
    image, mountpoint = checked_path(args.image), checked_path(args.mountpoint)
    if image == mountpoint or image in mountpoint.parents or mountpoint in image.parents:
        raise ValueError("Image and mountpoint must be separate paths")
    if args.action == "create" or (args.action == "init" and not image.exists()):
        create(image, args.size)
    if args.action in ["init", "mount"]:
        mount(image, mountpoint)
    if args.action in ["init", "install-service"]:
        install_service(image, mountpoint)
    if args.action == "uninstall-service":
        uninstall_service(image, mountpoint)
    # shortcut: successful mount exits; after a later unplug, kickstart this service to remount.
    print(f"Storage {args.action} completed; parent-volume settings and workload data are retained.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
