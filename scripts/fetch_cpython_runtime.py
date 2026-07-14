#!/usr/bin/env python3
"""Fetch a target-architecture CPython runtime from Alpine APK repos."""

# ─── How to run ───
# python3 scripts/fetch_cpython_runtime.py --arch riscv64 --dest user/tools/riscv64/tests/cpython
# python3 scripts/fetch_cpython_runtime.py --arch loongarch64 --dest user/tools/loongarch64/tests/cpython --dry-run

from __future__ import annotations

import argparse, io, os, posixpath, re, shutil, sys, tarfile
import urllib.error
import urllib.request
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from typing import Final, NoReturn

DEFAULT_MIRROR: Final = "https://dl-cdn.alpinelinux.org/alpine/edge/main"
ROOT_PACKAGES: Final = ("python3", "ca-certificates-bundle")
STAMP_NAME: Final = ".cpython-runtime.stamp"
DEP_VERSION_RE: Final = re.compile(r"[<>=~].*$")
SKIP_CONTROL: Final = (".SIGN.", ".PKGINFO", ".INSTALL")


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    dependencies: tuple[str, ...] = field(default_factory=tuple)
    provides: tuple[str, ...] = field(default_factory=tuple)
    compressed_size: int = 0
    installed_size: int = 0

    @property
    def filename(self) -> str:
        return f"{self.name}-{self.version}.apk"


class RuntimeFetchError(RuntimeError):
    """Expected user-facing failure."""


def fail(message: str) -> NoReturn:
    raise RuntimeFetchError(message)


def format_bytes(size: int) -> str:
    value = float(size)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if value < 1024.0:
            return f"{value:.2f} {unit}"
        value /= 1024.0
    return f"{value:.2f} TiB"


def normalize_dep(dep: str) -> str:
    return DEP_VERSION_RE.sub("", dep).strip()


def finish_package(fields: dict[str, str], packages: list[Package]) -> None:
    name = fields.get("P", "").strip()
    version = fields.get("V", "").strip()
    if not name or not version:
        return
    try:
        compressed_size = int(fields.get("S", "0"))
        installed_size = int(fields.get("I", "0"))
    except ValueError:
        compressed_size = 0
        installed_size = 0
    packages.append(Package(name, version, tuple(fields.get("D", "").split()), tuple(fields.get("p", "").split()), compressed_size, installed_size))


def parse_apkindex(text: str) -> list[Package]:
    packages: list[Package] = []
    fields: dict[str, str] = {}
    for line in text.splitlines():
        if not line:
            finish_package(fields, packages)
            fields = {}
            continue
        if len(line) >= 2 and line[1] == ":":
            fields[line[0]] = line[2:]
    finish_package(fields, packages)
    return packages


def download_bytes(url: str) -> bytes:
    try:
        with urllib.request.urlopen(url, timeout=60) as response:
            return response.read()
    except urllib.error.URLError as error:
        fail(f"network failure while downloading {url}: {error}")


def fetch_apkindex(mirror: str, arch: str) -> str:
    url = f"{mirror}/{arch}/APKINDEX.tar.gz"
    print(f"[cpython] Fetching APKINDEX from {url} ...")
    try:
        with tarfile.open(fileobj=io.BytesIO(download_bytes(url)), mode="r:gz") as archive:
            member = archive.getmember("APKINDEX")
            fileobj = archive.extractfile(member)
            if fileobj is None:
                fail("APKINDEX entry is not readable")
            return fileobj.read().decode("utf-8", errors="replace")
    except KeyError:
        fail("APKINDEX.tar.gz does not contain APKINDEX")
    except tarfile.TarError as error:
        fail(f"failed to parse APKINDEX.tar.gz: {error}")


def build_maps(packages: list[Package]) -> tuple[dict[str, Package], dict[str, str]]:
    by_name: dict[str, Package] = {}
    provides: dict[str, str] = {}
    for package in packages:
        current = by_name.get(package.name)
        if current is None or package.version > current.version:
            by_name[package.name] = package
    for package in by_name.values():
        provides.setdefault(package.name, package.name)
        for provided in package.provides:
            provides.setdefault(normalize_dep(provided), package.name)
    return by_name, provides


def resolve_dependency(dep: str, packages: dict[str, Package], provides: dict[str, str]) -> str:
    name = normalize_dep(dep)
    if name.startswith("!"):
        return ""
    if name.startswith("so:"):
        provider = provides.get(name)
        if provider is None:
            fail(f"missing provider for dependency {dep}")
        return provider
    if name not in packages:
        provider = provides.get(name)
        if provider is None:
            fail(f"missing package dependency {dep}")
        return provider
    return name


def resolve_closure(packages: dict[str, Package], provides: dict[str, str]) -> list[Package]:
    resolved: dict[str, Package] = {}
    queue: deque[str] = deque(ROOT_PACKAGES)
    while queue:
        name = resolve_dependency(queue.popleft(), packages, provides)
        if not name or name in resolved:
            continue
        package = packages.get(name)
        if package is None:
            fail(f"provider {name} is not present as a package")
        resolved[name] = package
        queue.extend(package.dependencies)
    return [resolved[name] for name in sorted(resolved)]


def print_summary(closure: list[Package], arch: str) -> None:
    total_download = sum(package.compressed_size for package in closure)
    total_installed = sum(package.installed_size for package in closure)
    print(f"[cpython] Resolving dependencies for {arch}...")
    print(f"[cpython] Root packages: {' '.join(ROOT_PACKAGES)}")
    print(f"[cpython] Dependency closure: {len(closure)} packages")
    print(
        f"[cpython] Total download: {format_bytes(total_download)}, "
        f"installed: {format_bytes(total_installed)}",
    )
    for package in closure:
        print(f"  {package.name}-{package.version}")


def stamp_is_valid(dest: Path, arch: str) -> bool:
    stamp = dest / STAMP_NAME
    if not stamp.is_file():
        return False
    try:
        return f"arch: {arch}" in stamp.read_text(encoding="ascii")
    except OSError:
        return False


def download_package(url: str, destination: Path) -> None:
    print(f"[cpython] [fetch] {destination.name}")
    tmp_file = destination.with_suffix(destination.suffix + ".tmp")
    try:
        with urllib.request.urlopen(url, timeout=60) as response, tmp_file.open("wb") as output:
            shutil.copyfileobj(response, output, length=1024 * 1024)
        tmp_file.replace(destination)
    except urllib.error.URLError as error:
        tmp_file.unlink(missing_ok=True)
        fail(f"network failure while downloading {url}: {error}")
    except OSError as error:
        tmp_file.unlink(missing_ok=True)
        fail(f"failed to write {destination}: {error}")


def safe_member_name(member: tarfile.TarInfo) -> PurePosixPath | None:
    name = member.name[2:] if member.name.startswith("./") else member.name
    name = name.lstrip("/")
    if not name or name.startswith(SKIP_CONTROL):
        return None
    normalized = PurePosixPath(posixpath.normpath(name))
    if normalized.is_absolute() or ".." in normalized.parts:
        fail(f"refusing unsafe apk path {member.name!r}")
    return normalized


def extract_member(archive: tarfile.TarFile, member: tarfile.TarInfo, dest: Path) -> int:
    rel_name = safe_member_name(member)
    if rel_name is None:
        return 0
    target = dest.joinpath(*rel_name.parts)
    if member.isdir():
        target.mkdir(parents=True, exist_ok=True)
    elif member.issym():
        target.parent.mkdir(parents=True, exist_ok=True)
        target.unlink(missing_ok=True)
        os.symlink(member.linkname, target)
    elif member.islnk():
        link_name = safe_member_name(tarfile.TarInfo(member.linkname))
        if link_name is None:
            fail(f"refusing unsafe hardlink target {member.linkname!r}")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.unlink(missing_ok=True)
        os.link(dest.joinpath(*link_name.parts), target)
    elif member.isfile():
        source = archive.extractfile(member)
        if source is None:
            fail(f"failed to read {member.name} from apk")
        target.parent.mkdir(parents=True, exist_ok=True)
        with source, target.open("wb") as output:
            shutil.copyfileobj(source, output)
        os.chmod(target, member.mode & 0o7777)
    return 1


def extract_apk(apk_path: Path, dest: Path) -> int:
    try:
        with tarfile.open(apk_path, mode="r:gz") as archive:
            return sum(extract_member(archive, member, dest) for member in archive)
    except tarfile.TarError as error:
        fail(f"failed to extract {apk_path.name}: {error}")
    except OSError as error:
        fail(f"failed to extract {apk_path.name}: {error}")


def write_stamp(dest: Path, arch: str) -> None:
    timestamp = datetime.now(timezone.utc).isoformat(timespec="seconds")
    (dest / STAMP_NAME).write_text(f"arch: {arch}\ntimestamp: {timestamp}\n", encoding="ascii")


def fetch_runtime(arch: str, dest: Path, mirror: str, force: bool, dry_run: bool) -> None:
    mirror = mirror.rstrip("/")
    if stamp_is_valid(dest, arch) and not force and not dry_run:
        print(f"[cpython] Cache hit: {dest} already has a valid runtime stamp")
        return
    packages, provides = build_maps(parse_apkindex(fetch_apkindex(mirror, arch)))
    closure = resolve_closure(packages, provides)
    print_summary(closure, arch)
    if dry_run:
        print("[cpython] Dry-run complete; no packages downloaded.")
        return
    dest.mkdir(parents=True, exist_ok=True)
    cache = dest / ".apk-cache"
    cache.mkdir(parents=True, exist_ok=True)
    for package in closure:
        apk_path = cache / package.filename
        if apk_path.is_file():
            print(f"[cpython] [cache] {package.filename}")
            continue
        download_package(f"{mirror}/{arch}/{package.filename}", apk_path)
    extracted = 0
    for index, package in enumerate(closure, start=1):
        print(f"[cpython] [extract {index}/{len(closure)}] {package.filename}")
        extracted += extract_apk(cache / package.filename, dest)
    write_stamp(dest, arch)
    print(f"[cpython] Extracted {extracted} filesystem entries to {dest}")
    print(f"[cpython] Wrote {dest / STAMP_NAME}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=("riscv64", "loongarch64"), required=True)
    parser.add_argument("--dest", type=Path, required=True)
    parser.add_argument("--mirror", default=DEFAULT_MIRROR)
    parser.add_argument("--force", action="store_true", help="ignore an existing runtime stamp")
    parser.add_argument("--dry-run", action="store_true", help="resolve dependencies without downloading")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        fetch_runtime(args.arch, args.dest, args.mirror, args.force, args.dry_run)
    except RuntimeFetchError as error:
        print(f"[cpython] ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
