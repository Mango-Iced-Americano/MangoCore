#!/usr/bin/env python3
"""Verify and atomically install MangoCore's canonical strict-aligned LA64 runtime."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import shutil
import tarfile
from pathlib import Path, PurePosixPath
from typing import BinaryIO


POLICY = "mangocore-la64-strict-align-v1"
TARGET = "loongarch64-linux-musl"
REQUIRED_FLAGS = {"-march=loongarch64", "-mabi=lp64d", "-mstrict-align"}
RUNTIME_INTERP = "/persist/python-runtime/current/lib/ld-musl-loongarch64.so.1"
REQUIRED_PATHS = {
    "lib/ld-musl-loongarch64.so.1",
    "usr/bin/python3",
    "python3-wrapper.sh",
    "strict_runtime_smoke.sh",
    "verify_runtime_integrity.py",
}


def sha256_stream(stream: BinaryIO) -> str:
    digest = hashlib.sha256()
    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(chunk)
    return digest.hexdigest()


def sha256_file(path: Path) -> str:
    with path.open("rb") as stream:
        return sha256_stream(stream)


def stays_below_root(candidate: PurePosixPath) -> bool:
    depth = 0
    for part in candidate.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if depth == 0:
                return False
            depth -= 1
        else:
            depth += 1
    return True


def validated_members(archive: tarfile.TarFile) -> dict[str, tarfile.TarInfo]:
    members: dict[str, tarfile.TarInfo] = {}
    for member in archive.getmembers():
        name = PurePosixPath(member.name)
        normalized = name.as_posix()
        if normalized in ("", ".") or name.is_absolute() or not stays_below_root(name):
            raise SystemExit(f"unsafe archive member: {member.name}")
        if normalized in members:
            raise SystemExit(f"duplicate archive member: {member.name}")
        if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
            raise SystemExit(f"unsupported special archive member: {member.name}")
        if member.issym() or member.islnk():
            target = PurePosixPath(member.linkname)
            if target.is_absolute():
                raise SystemExit(f"absolute archive link: {member.name}")
            resolved = name.parent / target if member.issym() else target
            if not stays_below_root(resolved):
                raise SystemExit(f"escaping archive link: {member.name}")
        members[normalized] = member
    return members


def read_json_member(
    archive: tarfile.TarFile, members: dict[str, tarfile.TarInfo], name: str
) -> dict[str, object]:
    member = members.get(name)
    if member is None or not member.isfile():
        raise SystemExit(f"missing archive member: {name}")
    stream = archive.extractfile(member)
    if stream is None:
        raise SystemExit(f"cannot read archive member: {name}")
    try:
        return json.load(stream)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"invalid {name}: {exc}") from exc


def validate_manifest(manifest: dict[str, object]) -> list[dict[str, object]]:
    flags = set(str(manifest.get("strict_flags", "")).split())
    if manifest.get("target") != TARGET:
        raise SystemExit(f"unexpected runtime target: {manifest.get('target')!r}")
    if not REQUIRED_FLAGS.issubset(flags):
        raise SystemExit("runtime manifest does not contain the complete strict-align flags")
    if manifest.get("pgo") is not True or manifest.get("lto") is not True:
        raise SystemExit("runtime must have both PGO and LTO enabled")
    if manifest.get("runtime_interpreter") != RUNTIME_INTERP:
        raise SystemExit("runtime PT_INTERP is not bound to the P4 current loader")
    elfs = manifest.get("elfs")
    if not isinstance(elfs, list) or not elfs:
        raise SystemExit("runtime manifest has no ELF closure")
    if manifest.get("elf_count") != len(elfs):
        raise SystemExit("runtime manifest ELF count is inconsistent")
    return elfs


def verify_archive(artifact: Path, expected_sha: str, expected_manifest_sha: str) -> dict[str, object]:
    actual_sha = sha256_file(artifact)
    if actual_sha != expected_sha:
        raise SystemExit(f"artifact sha256 mismatch: expected {expected_sha}, got {actual_sha}")
    sidecar = Path(str(artifact) + ".sha256")
    if not sidecar.is_file() or sidecar.read_text(encoding="utf-8").split()[0] != actual_sha:
        raise SystemExit(f"missing or inconsistent artifact sidecar: {sidecar}")

    with tarfile.open(artifact, "r:xz") as archive:
        members = validated_members(archive)
        missing = sorted(REQUIRED_PATHS - members.keys())
        if missing:
            raise SystemExit("runtime archive is incomplete: " + ", ".join(missing))
        manifest_member = members.get("strict-runtime-manifest.json")
        if manifest_member is None:
            raise SystemExit("runtime archive has no strict manifest")
        manifest_stream = archive.extractfile(manifest_member)
        if manifest_stream is None:
            raise SystemExit("cannot read strict runtime manifest")
        manifest_bytes = manifest_stream.read()
        manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
        if manifest_sha != expected_manifest_sha:
            raise SystemExit(
                f"manifest sha256 mismatch: expected {expected_manifest_sha}, got {manifest_sha}"
            )
        try:
            manifest = json.loads(manifest_bytes)
        except json.JSONDecodeError as exc:
            raise SystemExit(f"invalid strict runtime manifest: {exc}") from exc
        elfs = validate_manifest(manifest)
        for entry in elfs:
            if not isinstance(entry, dict):
                raise SystemExit("invalid ELF manifest entry")
            name = str(entry.get("path", ""))
            expected = str(entry.get("sha256", ""))
            member = members.get(name)
            if member is None or not member.isfile():
                raise SystemExit(f"missing manifest ELF: {name}")
            stream = archive.extractfile(member)
            if stream is None or sha256_stream(stream) != expected:
                raise SystemExit(f"ELF sha256 mismatch: {name}")
            interpreter = entry.get("interpreter")
            if interpreter not in (None, RUNTIME_INTERP):
                raise SystemExit(f"non-P4 ELF interpreter: {name}: {interpreter!r}")
    return manifest


def remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.exists():
        shutil.rmtree(path)


def install(artifact: Path, destination: Path, index: dict[str, object]) -> None:
    destination = Path(os.path.abspath(destination))
    parent = destination.parent
    parent.mkdir(parents=True, exist_ok=True)
    staging = parent / f".{destination.name}.strict-staging"
    backup = parent / f".{destination.name}.strict-backup"
    remove_path(staging)
    destination_exists = destination.exists() or destination.is_symlink()
    backup_exists = backup.exists() or backup.is_symlink()
    if backup_exists and not destination_exists:
        os.replace(backup, destination)
    elif backup_exists:
        remove_path(backup)
    staging.mkdir(mode=0o755)
    try:
        with tarfile.open(artifact, "r:xz") as archive:
            validated_members(archive)
            archive.extractall(staging)
        stamp = {
            "schema": 1,
            "arch": "loongarch64",
            "runtime_policy": POLICY,
            "artifact": artifact.name,
            "artifact_sha256": index["sha256"],
            "manifest_sha256": index["manifest_sha256"],
            "installed_utc": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
        }
        (staging / ".cpython-runtime.stamp").write_text(
            json.dumps(stamp, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        for required in REQUIRED_PATHS:
            if not (staging / required).exists():
                raise SystemExit(f"extracted runtime is incomplete: {required}")
        had_destination = destination.exists() or destination.is_symlink()
        if had_destination:
            os.replace(destination, backup)
        try:
            os.replace(staging, destination)
        except BaseException:
            if had_destination and backup.exists():
                os.replace(backup, destination)
            raise
        remove_path(backup)
    finally:
        remove_path(staging)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact-index", type=Path, required=True)
    parser.add_argument("--dest", type=Path)
    parser.add_argument("--verify-only", action="store_true")
    args = parser.parse_args()
    if not args.verify_only and args.dest is None:
        parser.error("--dest is required unless --verify-only is used")

    index_path = args.artifact_index.resolve()
    try:
        index = json.loads(index_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        parser.error(f"cannot read artifact index {index_path}: {exc}")
    if index.get("runtime_policy") != POLICY:
        raise SystemExit(f"unexpected runtime policy: {index.get('runtime_policy')!r}")
    artifact_name = str(index.get("artifact", ""))
    if PurePosixPath(artifact_name).name != artifact_name:
        raise SystemExit("artifact index contains an unsafe filename")
    artifact = index_path.parent / artifact_name
    manifest = verify_archive(
        artifact,
        str(index.get("sha256", "")),
        str(index.get("manifest_sha256", "")),
    )
    print(f"runtime_policy={POLICY}")
    print(f"artifact={artifact}")
    print(f"artifact_sha256={index['sha256']}")
    print(f"manifest_schema={manifest.get('schema', 1)}")
    print(f"elf_count={manifest['elf_count']}")
    if args.verify_only:
        return 0
    assert args.dest is not None
    install(artifact, args.dest, index)
    print(f"installed={args.dest.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
