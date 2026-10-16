"""Validate a release identity once for desktop and container publication."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import tomllib


VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")


def release_metadata(root: Path, event: str, ref: str, signing: str, sha: str) -> dict[str, str]:
    publish = event == "push" and ref.startswith("refs/tags/")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("Release builds require an immutable commit SHA")
    if not publish:
        mode = signing or "unsigned"
        if mode not in {"signed", "unsigned"}:
            raise ValueError("RELEASE_SIGNING must be signed or unsigned")
        return dict(sha=sha, publish="false", version="", prerelease="false", signing=mode)

    tag = ref.removeprefix("refs/tags/")
    match = VERSION.fullmatch(tag.removeprefix("v")) if tag.startswith("v") else None
    if not match or any(part.isdigit() and len(part) > 1 and part.startswith("0")
                        for part in (match[4] or "").split(".")):
        raise ValueError("Release tags must be vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-prerelease (no build metadata)")
    version = tag[1:]
    if signing not in {"signed", "unsigned"}:
        raise ValueError("Set the RELEASE_SIGNING repository variable to signed or unsigned before publishing")
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    ui = json.loads((root / "ui/package.json").read_text())
    lock = json.loads((root / "ui/package-lock.json").read_text())
    versions = {
        "Cargo workspace": cargo["workspace"]["package"]["version"],
        "UI package": ui["version"],
        "UI lockfile": lock["version"],
        "UI lockfile root": lock["packages"][""]["version"],
    }
    members = {tomllib.loads((root / member / "Cargo.toml").read_text())["package"]["name"]
               for member in cargo["workspace"]["members"]}
    cargo_lock = tomllib.loads((root / "Cargo.lock").read_text())
    for package in cargo_lock["package"]:
        if package["name"] in members and "source" not in package:
            versions[f"Cargo lockfile {package['name']}"] = package["version"]
    tauri = json.loads((root / "crates/desktop/tauri.conf.json").read_text())
    if "version" in tauri:
        versions["Tauri override"] = tauri["version"]
    mismatches = [f"{name}={value}" for name, value in versions.items() if value != version]
    if mismatches:
        raise ValueError(f"Tag {tag} does not match release versions: {', '.join(mismatches)}")
    return dict(sha=sha, publish="true", version=version,
                prerelease=str(match[4] is not None).lower(), signing=signing)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--signing", default="")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    try:
        metadata = release_metadata(root, args.event, args.ref, args.signing, sha)
    except ValueError as error:
        parser.error(str(error))
    with args.output.open("a") as output:
        for key, value in metadata.items():
            output.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
