"""Reject incomplete desktop build output before artifact upload."""

import argparse
from pathlib import Path
import zipfile


PATTERNS = {
    "aarch64-apple-darwin": ("dmg/*.dmg",),
    "x86_64-unknown-linux-gnu": ("deb/*.deb", "appimage/*.AppImage"),
    "x86_64-pc-windows-msvc": ("nsis/*.exe", "msi/*.msi", "portable/*.zip"),
}


def verify_bundles(bundle_dir: Path, target: str) -> None:
    for pattern in PATTERNS[target]:
        files = list(bundle_dir.glob(pattern))
        if len(files) != 1 or files[0].stat().st_size == 0:
            raise ValueError(f"Expected exactly one nonempty {pattern}, found {files}")
    if target == "x86_64-pc-windows-msvc":
        with zipfile.ZipFile(next(bundle_dir.glob("portable/*.zip"))) as archive:
            names = set(archive.namelist())
            required = {"Wilkes.exe", "wilkes-rust-worker.exe", "requirements.txt",
                        "wilkes_python_worker/__main__.py"}
            missing = required - names
            if missing:
                raise ValueError(f"Portable archive is missing {sorted(missing)}")
            bad = archive.testzip()
            if bad:
                raise ValueError(f"Corrupt portable archive entry: {bad}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=PATTERNS)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    verify_bundles(root / "target" / args.target / "release/bundle", args.target)


if __name__ == "__main__":
    main()
