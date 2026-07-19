#!/usr/bin/env python3
"""Apply the reviewed smolagents 1.26.0 interactive CLI fixes on P4."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import tempfile


EXPECTED_NAME = "smolagents"
EXPECTED_VERSION = "1.26.0"
ORIGINAL_SHA256 = "c7cd04f6312242fbdb16917c48b9b5a672cb5a0652f9553c718b68dd3e2b5d62"
ACTION_ONLY_PATCHED_SHA256 = "9c3735c6aff445fe01a064f0ab61d4280e36588a1952c8f0220d3ecf8e563a57"
PATCHED_SHA256 = "e4052f70bb355b35ec3a9720475a22e898574444d024f4e8a38af41e05de7eba"
OLD_BLOCK = b'''    imports = []\n    action_type = "code"\n\n    if Confirm.ask("\\n[bold white]Configure advanced options?[/]", default=False):\n'''
NEW_BLOCK = b'''    imports = []\n    # Preserve the action type selected at the start of interactive_mode().\n\n    if Confirm.ask("\\n[bold white]Configure advanced options?[/]", default=False):\n'''
OLD_MODEL_BLOCK = b'''    if model_type == "OpenAIModel":\n'''
NEW_MODEL_BLOCK = b'''    if model_type in ("OpenAIModel", "OpenAIServerModel"):\n'''


def distribution_version(site: Path) -> str | None:
    matches = sorted(site.glob("smolagents-*.dist-info/METADATA"))
    if not matches:
        return None
    if len(matches) != 1:
        raise RuntimeError("ambiguous smolagents metadata under %s" % site)
    name = None
    version = None
    with matches[0].open("r", encoding="utf-8") as stream:
        for line in stream:
            if line.startswith("Name: "):
                name = line[6:].strip()
            elif line.startswith("Version: "):
                version = line[9:].strip()
            if name is not None and version is not None:
                break
    if name != EXPECTED_NAME or version is None:
        raise RuntimeError("invalid smolagents distribution metadata")
    return version


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def atomic_publish(path: Path, data: bytes, expected_sha256: str) -> None:
    """Publish complete data without ever truncating the active file."""
    if digest(data) != expected_sha256:
        raise RuntimeError("refusing to publish data with an unexpected digest")
    fd, temporary_name = tempfile.mkstemp(
        prefix=".%s.mango-" % path.name, suffix=".tmp", dir=path.parent
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "wb") as stream:
            os.fchmod(stream.fileno(), 0o644)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        if digest(temporary.read_bytes()) != expected_sha256:
            raise RuntimeError("temporary file verification failed")
        os.replace(temporary, path)
        # MangoCore's current ext4 does not yet guarantee that fsync(directory)
        # is available.  A global sync makes the rename durable; if reset hits
        # earlier, the old file remains addressable and the next boot retries.
        os.sync()
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def reviewed_original(data: bytes) -> bool:
    return (
        digest(data) == ORIGINAL_SHA256
        and data.count(OLD_BLOCK) == 1
        and data.count(OLD_MODEL_BLOCK) == 1
    )


def reviewed_action_only_patched(data: bytes) -> bool:
    return (
        digest(data) == ACTION_ONLY_PATCHED_SHA256
        and data.count(NEW_BLOCK) == 1
        and data.count(OLD_MODEL_BLOCK) == 1
    )


def reviewed_patched(data: bytes) -> bool:
    return (
        digest(data) == PATCHED_SHA256
        and data.count(NEW_BLOCK) == 1
        and data.count(NEW_MODEL_BLOCK) == 1
    )


def build_patched(original: bytes) -> bytes:
    if not reviewed_original(original):
        raise RuntimeError("refusing to patch unreviewed smolagents source")
    patched = original.replace(OLD_BLOCK, NEW_BLOCK, 1)
    patched = patched.replace(OLD_MODEL_BLOCK, NEW_MODEL_BLOCK, 1)
    if not reviewed_patched(patched):
        raise RuntimeError("constructed smolagents patch has an unexpected digest")
    return patched


def cache_paths(source: Path) -> list[Path]:
    directories = {source.parent / "__pycache__"}
    directories.add(Path(importlib.util.cache_from_source(str(source))).parent)
    result = []
    for directory in directories:
        if directory.is_dir():
            result.extend(directory.glob("%s.*.pyc" % source.stem))
    return sorted(set(result))


def clear_generated_state(source: Path, backup: Path) -> None:
    changed = False
    for pyc in cache_paths(source):
        pyc.unlink()
        changed = True
    for target in (source, backup):
        for stale in source.parent.glob(".%s.mango-*.tmp" % target.name):
            stale.unlink()
            changed = True
    if changed:
        os.sync()


def patch(site: Path, *, check: bool, allow_missing: bool) -> str:
    site = site.resolve()
    package = site / "smolagents"
    version = distribution_version(site)
    if version is None:
        if allow_missing and not package.exists():
            return "missing"
        if package.exists():
            raise RuntimeError("smolagents package exists without reviewed metadata")
        raise RuntimeError("smolagents is not installed under %s" % site)
    if version != EXPECTED_VERSION:
        raise RuntimeError(
            "refusing unreviewed smolagents version %s (expected %s)"
            % (version, EXPECTED_VERSION)
        )

    if package.is_symlink():
        raise RuntimeError("smolagents package directory must not be a symlink")
    package = package.resolve()
    if package.parent != site:
        raise RuntimeError("smolagents package directory escapes the P4 user site")
    source = package / "cli.py"
    if source.parent != package or source.is_symlink():
        raise RuntimeError("smolagents cli.py escapes the user site")
    backup = source.with_name("cli.py.mango-1.26.0.orig")
    if backup.is_symlink():
        raise RuntimeError("smolagents recovery backup must not be a symlink")
    data = source.read_bytes() if source.is_file() else None
    backup_data = backup.read_bytes() if backup.is_file() else None

    if data is not None and reviewed_patched(data):
        if check:
            if backup_data is None or not reviewed_original(backup_data):
                raise RuntimeError("reviewed recovery backup is missing or corrupt")
            if cache_paths(source):
                raise RuntimeError("unsafe generated cli.py bytecode remains on P4")
            return "already-patched"
        if backup_data is None:
            original = data.replace(NEW_BLOCK, OLD_BLOCK, 1)
            original = original.replace(NEW_MODEL_BLOCK, OLD_MODEL_BLOCK, 1)
            atomic_publish(backup, original, ORIGINAL_SHA256)
        elif not reviewed_original(backup_data):
            raise RuntimeError("smolagents recovery backup has an unexpected digest")
        status = "already-patched"
        original = None

    elif data is not None and reviewed_action_only_patched(data):
        if check:
            raise RuntimeError(
                "OpenAIServerModel is offered by the menu but unsupported by load_model"
            )
        if backup_data is None:
            original = data.replace(NEW_BLOCK, OLD_BLOCK, 1)
            atomic_publish(backup, original, ORIGINAL_SHA256)
        elif not reviewed_original(backup_data):
            raise RuntimeError("smolagents recovery backup has an unexpected digest")
        else:
            original = backup_data
        status = "upgraded-model-alias"

    elif data is not None and reviewed_original(data):
        if check:
            raise RuntimeError("smolagents interactive CLI fixes are not installed")
        if backup_data is None:
            atomic_publish(backup, data, ORIGINAL_SHA256)
        elif not reviewed_original(backup_data):
            raise RuntimeError("smolagents recovery backup has an unexpected digest")
        status = "patched"
        original = data
    else:
        if check:
            raise RuntimeError(
                "smolagents cli.py is corrupt or is not the reviewed 1.26.0 source"
            )
        if backup_data is None or not reviewed_original(backup_data):
            raise RuntimeError(
                "smolagents cli.py is corrupt and no reviewed P4 recovery backup exists"
            )
        # A real-board reset exposed cli.py containing a CPython .pyc header.
        # Recover only from the exact official source retained beside it.
        original = backup_data
        status = "recovered-patched"

    if original is not None:
        patched = build_patched(original)
        atomic_publish(source, patched, PATCHED_SHA256)
    if not reviewed_patched(source.read_bytes()):
        raise RuntimeError("smolagents cli.py verification failed after publication")
    clear_generated_state(source, backup)
    return status


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--site",
        type=Path,
        default=Path("/persist/python/user/lib/python3.14/site-packages"),
    )
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--allow-missing", action="store_true")
    args = parser.parse_args()
    try:
        status = patch(args.site, check=args.check, allow_missing=args.allow_missing)
    except (OSError, RuntimeError) as exc:
        parser.exit(1, "[smolagents-action-type] FAIL: %s\n" % exc)
    print(
        "[smolagents-action-type] status=%s version=%s site=%s"
        % (status, EXPECTED_VERSION, args.site)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
