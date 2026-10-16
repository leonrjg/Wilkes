"""Publication-boundary regression tests. Run with unittest discovery."""

import json
from pathlib import Path
import tempfile
import unittest

from release_metadata import release_metadata


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sha = "a" * 40
        self.version("1.0.0")

    def version(self, version):
        for folder in ("ui", "crates/desktop"):
            (self.root / folder).mkdir(parents=True, exist_ok=True)
        (self.root / "Cargo.toml").write_text(
            f'[workspace]\nmembers = ["crates/desktop"]\n[workspace.package]\nversion = "{version}"\n')
        (self.root / "Cargo.lock").write_text(
            f'[[package]]\nname = "wilkes-desktop"\nversion = "{version}"\n')
        (self.root / "crates/desktop/Cargo.toml").write_text('[package]\nname = "wilkes-desktop"\n')
        (self.root / "crates/desktop/tauri.conf.json").write_text('{}')
        (self.root / "ui/package.json").write_text(json.dumps(dict(version=version)))
        (self.root / "ui/package-lock.json").write_text(json.dumps(
            dict(version=version, packages={"": dict(version=version)})))

    def metadata(self, ref="refs/tags/v1.0.0", event="push", signing="unsigned"):
        return release_metadata(self.root, event, ref, signing, self.sha)

    def test_stable(self):
        result = self.metadata()
        self.assertEqual(result["publish"], "true")
        self.assertEqual(result["prerelease"], "false")
        self.assertEqual(result["sha"], self.sha)
        self.assertEqual(result["version"], "1.0.0")

    def test_prerelease(self):
        self.version("1.0.0-rc.1")
        result = self.metadata(ref="refs/tags/v1.0.0-rc.1")
        self.assertEqual(result["prerelease"], "true")
        self.assertEqual(result["version"], "1.0.0-rc.1")

    def test_manual_run_never_publishes_even_on_tag(self):
        self.assertEqual(self.metadata(event="workflow_dispatch")["publish"], "false")

    def test_release_signing_must_be_explicit(self):
        with self.assertRaisesRegex(ValueError, "RELEASE_SIGNING"):
            self.metadata(signing="")
        self.assertEqual(self.metadata(signing="signed")["signing"], "signed")

    def test_manual_unsigned_build_is_explicit_in_metadata(self):
        self.assertEqual(self.metadata(event="workflow_dispatch", signing="")["signing"], "unsigned")

    def test_invalid_tags(self):
        for tag in ("v1", "v01.0.0", "v1.0.0-rc.01", "v1.0.0+build", "v1.0.0-", "v1.0.0/bad"):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "Release tags"):
                self.metadata(ref=f"refs/tags/{tag}")

    def test_version_mismatch(self):
        for filename, data in (
            ("ui/package.json", '{"version":"0.9.5"}'),
            ("crates/desktop/tauri.conf.json", '{"version":"0.9.5"}'),
            ("Cargo.lock", '[[package]]\nname = "wilkes-desktop"\nversion = "0.9.5"\n'),
        ):
            with self.subTest(filename=filename):
                self.version("1.0.0")
                (self.root / filename).write_text(data)
                with self.assertRaisesRegex(ValueError, "does not match"):
                    self.metadata()

    def test_mutable_ref_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "immutable"):
            release_metadata(self.root, "push", "refs/tags/v1.0.0", "unsigned", "main")


if __name__ == "__main__":
    unittest.main()
