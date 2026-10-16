import tempfile
from pathlib import Path
import unittest
import zipfile

from verify_release_bundles import verify_bundles


class ReleaseBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, path):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_bytes(b"bundle")
        return file

    def test_linux_requires_both_installers(self):
        self.write("deb/Wilkes.deb")
        with self.assertRaisesRegex(ValueError, "AppImage"):
            verify_bundles(self.root, "x86_64-unknown-linux-gnu")
        self.write("appimage/Wilkes.AppImage")
        verify_bundles(self.root, "x86_64-unknown-linux-gnu")

    def test_cached_duplicate_bundle_is_rejected(self):
        self.write("dmg/Wilkes_0.9.5.dmg")
        self.write("dmg/Wilkes_1.0.0.dmg")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            verify_bundles(self.root, "aarch64-apple-darwin")

    def test_empty_bundle_is_rejected(self):
        self.write("dmg/Wilkes.dmg").write_bytes(b"")
        with self.assertRaisesRegex(ValueError, "nonempty"):
            verify_bundles(self.root, "aarch64-apple-darwin")

    def test_portable_archive_requires_real_worker_and_python_resources(self):
        self.write("nsis/Wilkes.exe")
        self.write("msi/Wilkes.msi")
        archive_path = self.write("portable/Wilkes.zip")
        with zipfile.ZipFile(archive_path, "w") as archive:
            archive.writestr("Wilkes.exe", b"exe")
        with self.assertRaisesRegex(ValueError, "missing"):
            verify_bundles(self.root, "x86_64-pc-windows-msvc")
        with zipfile.ZipFile(archive_path, "a") as archive:
            for name in ("wilkes-rust-worker.exe", "requirements.txt", "wilkes_python_worker/__main__.py"):
                archive.writestr(name, b"resource")
        verify_bundles(self.root, "x86_64-pc-windows-msvc")


if __name__ == "__main__":
    unittest.main()
