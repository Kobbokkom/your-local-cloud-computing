import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("macos_storage", Path(__file__).resolve().parents[1] / "scripts/macos-storage.py")
storage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(storage)


class StorageSafety(unittest.TestCase):
    def test_identical_existing_helper_cannot_bypass_parent_trust(self):
        with tempfile.TemporaryDirectory() as tmp:
            destination = Path(tmp).resolve() / "helper.py"
            destination.write_bytes(b"same")
            with patch.object(storage, "protected_file"):
                with self.assertRaises(ValueError):
                    storage.install_file(destination, b"same", 0o755)

    def test_refuses_noncanonical_or_symlink_path(self):
        for value in ("/", "relative", "/a/../b", "/a//b", "/a\nb"):
            with self.assertRaises(ValueError):
                storage.checked_path(value)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            (root / "link").symlink_to(root)
            with self.assertRaises(ValueError):
                storage.checked_path(str(root / "link" / "image.sparsebundle"))

    def test_refuses_another_image_at_mountpoint(self):
        info = {"images": [{"image-path": "/storage/other.sparsebundle", "system-entities": [{"mount-point": "/Volumes/YLCC"}]}]}
        with self.assertRaises(ValueError):
            storage.mounted_image(info, Path("/storage/ours.sparsebundle"), Path("/Volumes/YLCC"))

    def test_existing_image_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / "existing.sparsebundle"
            image.mkdir()
            (image / "valuable").write_text("keep")
            with patch.object(storage, "run") as command:
                with self.assertRaises(ValueError):
                    storage.create(image, "64g")
                command.assert_not_called()
            self.assertEqual((image / "valuable").read_text(), "keep")

    def test_mount_does_not_accept_ignored_ownership(self):
        with tempfile.TemporaryDirectory() as tmp:
            image = Path(tmp) / "ours.sparsebundle"
            image.mkdir()
            point = Path("/Volumes/YLCC")
            info = {"images": [{"image-path": str(image), "system-entities": [{"mount-point": str(point)}]}]}
            with patch.object(storage, "run", side_effect=[info, {"GlobalPermissionsEnabled": False}]) as command:
                with self.assertRaises(RuntimeError):
                    storage.mount(image, point)
                self.assertEqual(command.call_count, 2)


if __name__ == "__main__":
    unittest.main()
