#!/usr/bin/env python3
"""Hold Cargo's target directory under a size limit.

Cargo never deletes anything from `target/`, and it keeps no record of which
artifacts a build used. Access times are no substitute: this is run on volumes
that do not update them on read, which is why `cargo sweep` is not what does
this job -- it would evict in creation order and take the live dependency tree
first.

So when the directory is over the limit, eviction goes by what it costs to get
back:

1. Incremental caches, least recently compiled first. rustc rewrites a crate's
   cache every time it compiles that crate, so the cache directory's mtime *is*
   a last-use time -- the one trustworthy one in here. Losing a cache costs a
   single cold compile of that crate.
2. Whole profile directories, largest first. Nothing finer can be told apart
   safely. Losing one costs a cold build of that profile.

A profile directory is only touched while holding Cargo's own build lock on it,
so a build in another session is never pulled out from under itself. A profile
that is busy is skipped, and said to be.
"""

from __future__ import annotations

import argparse
import fcntl
import os
import shutil
import sys
import time
from pathlib import Path

LIMIT_GB = 20

# Cargo takes an exclusive flock on this file for the length of a build.
BUILD_LOCK = ".cargo-lock"


def disk_usage(root: Path) -> int:
    """Bytes allocated under `root`, counting a hard-linked file once, as `du` does."""
    total = 0
    seen = set()
    stack = [root]
    while stack:
        with os.scandir(stack.pop()) as entries:
            for entry in entries:
                if entry.is_dir(follow_symlinks=False):
                    stack.append(entry.path)
                    continue
                stat = entry.stat(follow_symlinks=False)
                if stat.st_nlink > 1:
                    inode = (stat.st_dev, stat.st_ino)
                    if inode in seen:
                        continue
                    seen.add(inode)
                total += stat.st_blocks * 512
    return total


def profile_dirs(target: Path) -> list[Path]:
    """Every directory Cargo builds into: `debug`, `release`, `<triple>/release`, ..."""
    # Globbed to the depths Cargo uses rather than searched for: a recursive
    # walk would descend into every build script's output on every commit.
    depths = (f"{'*/' * depth}{BUILD_LOCK}" for depth in (1, 2, 3))
    return sorted(lock.parent for pattern in depths for lock in target.glob(pattern))


def try_lock(profile: Path) -> int | None:
    """Take Cargo's build lock on `profile` without waiting; None if a build holds it."""
    fd = os.open(profile / BUILD_LOCK, os.O_RDWR)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        os.close(fd)
        return None
    return fd


def incremental_caches(profile: Path) -> list[Path]:
    incremental = profile / "incremental"
    if not incremental.is_dir():
        return []
    return [cache for cache in incremental.iterdir() if cache.is_dir()]


def clear_profile(profile: Path) -> None:
    """Empty a profile directory, leaving the lock file this process holds."""
    for child in profile.iterdir():
        if child.name == BUILD_LOCK:
            continue
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()


def gb(size: int) -> str:
    return f"{size / 2**30:.1f} GB" if size >= 2**30 else f"{size / 2**20:.0f} MB"


def age(mtime: float) -> str:
    hours = (time.time() - mtime) / 3600
    return f"{hours / 24:.0f} d ago" if hours >= 48 else f"{hours:.0f} h ago"


def limit(target: Path, limit_bytes: int, dry_run: bool) -> bool:
    """Evict until `target` fits. Returns whether it does."""
    total = disk_usage(target)
    if total <= limit_bytes:
        if dry_run:
            print(f"{target}: {gb(total)}, within the {gb(limit_bytes)} limit")
        return True
    print(f"{target}: {gb(total)}, over the {gb(limit_bytes)} limit")
    verb = "would remove" if dry_run else "removed"

    locks = {}
    for profile in profile_dirs(target):
        fd = try_lock(profile)
        if fd is None:
            print(f"  skipped {profile.relative_to(target)}: a cargo build holds its lock")
        else:
            locks[profile] = fd

    # What each profile still holds, kept current as its caches go, so that
    # clearing a profile afterwards is not credited with them a second time.
    sizes = {profile: disk_usage(profile) for profile in locks}

    caches = [(cache, profile) for profile in locks for cache in incremental_caches(profile)]
    caches.sort(key=lambda entry: entry[0].stat().st_mtime)
    for cache, profile in caches:
        if total <= limit_bytes:
            break
        size = disk_usage(cache)
        last_compiled = cache.stat().st_mtime
        if not dry_run:
            shutil.rmtree(cache)
        total -= size
        sizes[profile] -= size
        print(f"  {verb} incremental cache {cache.relative_to(target)} "
              f"({gb(size)}, last compiled {age(last_compiled)})")

    for profile in sorted(locks, key=sizes.get, reverse=True):
        if total <= limit_bytes or not sizes[profile]:
            break
        if not dry_run:
            clear_profile(profile)
        total -= sizes[profile]
        print(f"  {verb} everything else in {profile.relative_to(target)} ({gb(sizes[profile])})")

    for fd in locks.values():
        os.close(fd)

    if not dry_run:
        # Measured again rather than trusted: the running total subtracts each
        # eviction's own size, which overstates what a hard link shared with
        # something still standing gives back.
        total = disk_usage(target)
    if total > limit_bytes:
        print(f"{target}: still {gb(total)} after evicting everything no running build holds",
              file=sys.stderr)
        return False
    print(f"{target}: {gb(total)}")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("target", nargs="?", type=Path,
                        default=Path(__file__).resolve().parent.parent / "target")
    parser.add_argument("--limit-gb", type=float, default=LIMIT_GB)
    parser.add_argument("--dry-run", action="store_true",
                        help="report what would be evicted without deleting it")
    args = parser.parse_args()
    if not args.target.is_dir():
        return 0
    fits = limit(args.target, int(args.limit_gb * 2**30), args.dry_run)
    return 0 if fits else 1


if __name__ == "__main__":
    sys.exit(main())
