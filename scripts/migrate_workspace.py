#!/usr/bin/env python3
"""One-off migration from Wilkes' alpha global library to a Default workspace.

The application intentionally contains no legacy compatibility path. Run this
script once, with Wilkes closed, before launching a workspace-enabled build. It
is safe to rerun after interruption: the chosen workspace UUID and manifest
are recorded until completion.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import sys
import uuid
from pathlib import Path
from typing import Any


REGISTRY_VERSION = 1
MANIFEST_VERSION = 1
MIGRATION_PLAN = ".workspace-migration.json"
LIBRARY_ENTRIES = (
    "semantic_index.db",
    "semantic_index.db-wal",
    "semantic_index.db-shm",
    "semantic_index.db.tmp",
    "semantic_index.db.tmp-wal",
    "semantic_index.db.tmp-shm",
    "semantic_index.status.json",
    "file_metadata.db",
    "file_metadata.db-wal",
    "file_metadata.db-shm",
    "research.db",
    "research.db-wal",
    "research.db-shm",
    "chat-conversations.json",
    "bookmarks.json",
    "bookmarks.json.migrated",
    "uploads",
)


def default_paths() -> tuple[Path, Path]:
    home = Path.home()
    system = platform.system()
    if system == "Darwin":
        base = home / "Library" / "Application Support" / "app.wilkes"
        return base, base
    if system == "Windows":
        base = Path(os.environ.get("APPDATA", home / "AppData" / "Roaming")) / "app.wilkes"
        return base, base
    data = Path(os.environ.get("XDG_DATA_HOME", home / ".local" / "share")) / "app.wilkes"
    config = Path(os.environ.get("XDG_CONFIG_HOME", home / ".config")) / "app.wilkes"
    return data, config


def read_json(path: Path, default: Any) -> Any:
    if not path.exists():
        return default
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def atomic_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)


def main() -> int:
    default_data, default_config = default_paths()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=default_data)
    parser.add_argument("--config-dir", type=Path, default=default_config)
    parser.add_argument("--name", default="Default")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    data_dir = args.data_dir.expanduser().resolve()
    config_dir = args.config_dir.expanduser().resolve()
    registry_path = data_dir / "workspaces.json"
    plan_path = data_dir / MIGRATION_PLAN
    if registry_path.exists():
        print(f"Workspace registry already exists: {registry_path}")
        return 0

    settings_path = config_dir / "settings.json"
    settings = read_json(settings_path, {})
    plan = read_json(plan_path, None)
    planned_id = plan.get("workspace_id") if isinstance(plan, dict) else None
    workspace_id = planned_id if isinstance(planned_id, str) and planned_id else str(uuid.uuid4())
    workspace_dir = data_dir / "workspaces" / workspace_id
    manifest_path = workspace_dir / "workspace.json"

    planned_manifest = plan.get("manifest") if isinstance(plan, dict) else None
    existing_manifest = read_json(manifest_path, None)
    if isinstance(planned_manifest, dict):
        manifest = planned_manifest
    elif isinstance(existing_manifest, dict):
        # An older interrupted run may have recorded only the UUID. The
        # manifest is written before global settings are cleaned, so it is the
        # authoritative recovery source in that case.
        manifest = existing_manifest
    else:
        favorites = settings.get("favorites", settings.get("bookmarked_dirs", []))
        recent_roots = settings.get("recent_dirs", [])
        active_root = settings.get("last_directory")
        semantic = settings.get("semantic")
        manifest = {
            "version": MANIFEST_VERSION,
            "id": workspace_id,
            "name": args.name.strip() or "Default",
            "favorites": favorites if isinstance(favorites, list) else [],
            "recent_roots": recent_roots if isinstance(recent_roots, list) else [],
            "active_root": active_root,
            "semantic": semantic,
        }
    if manifest.get("id") != workspace_id:
        raise RuntimeError("Migration manifest does not match its recorded workspace UUID")

    sources: list[tuple[Path, Path]] = []
    for filename in LIBRARY_ENTRIES:
        destination = workspace_dir / filename
        candidates = [
            source_dir / filename
            for source_dir in dict.fromkeys((data_dir, config_dir))
            if (source_dir / filename).exists()
            and (source_dir / filename) != destination
        ]
        if len(candidates) > 1:
            joined = ", ".join(str(path) for path in candidates)
            raise RuntimeError(
                f"Multiple legacy copies of {filename} exist; refusing to choose: {joined}"
            )
        if candidates:
            sources.append((candidates[0], destination))

    print(f"Data directory:   {data_dir}")
    print(f"Config directory: {config_dir}")
    print(f"Workspace:        {manifest['name']} ({workspace_id})")
    for source, destination in sources:
        print(f"move {source} -> {destination}")
    print(f"write {manifest_path}")
    print(f"write {registry_path}")
    if args.dry_run:
        return 0

    data_dir.mkdir(parents=True, exist_ok=True)
    atomic_json(plan_path, {"workspace_id": workspace_id, "manifest": manifest})
    workspace_dir.mkdir(parents=True, exist_ok=True)
    for source, destination in sources:
        if destination.exists():
            if source.exists():
                raise RuntimeError(
                    f"Both source and destination exist; refusing to overwrite: {source}, {destination}"
                )
            continue
        shutil.move(str(source), str(destination))

    atomic_json(manifest_path, manifest)
    cleaned_settings = dict(settings)
    for key in (
        "favorites",
        "bookmarked_dirs",
        "recent_dirs",
        "last_directory",
        "semantic",
    ):
        cleaned_settings.pop(key, None)
    if settings_path.exists() or cleaned_settings:
        atomic_json(settings_path, cleaned_settings)
    atomic_json(
        registry_path,
        {
            "version": REGISTRY_VERSION,
            "active_workspace_id": workspace_id,
            "workspace_ids": [workspace_id],
        },
    )
    plan_path.unlink(missing_ok=True)
    print("Migration complete.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"Migration failed: {error}", file=sys.stderr)
        raise SystemExit(1)
