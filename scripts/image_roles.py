#!/usr/bin/env python3
"""Load and enforce the image-role contract shared by Make and Python."""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path


class RoleContractError(RuntimeError):
    """Raised when an image path violates the role manifest."""


_ASSIGNMENT = re.compile(r"^\s*(IMAGE_ROLE_[A-Z0-9_]+)\s*(?::|\?|\+)?=\s*(.*?)\s*$")
_MAKE_VARIABLE = re.compile(r"\$\(([A-Z0-9_]+)\)")
_ARCHES = frozenset({"rv64", "la64"})


def _ensure_arch(arch: str) -> str:
    """Return a supported canonical architecture name."""
    if arch not in _ARCHES:
        raise RoleContractError(f"unsupported image-role architecture: {arch}")
    return arch


def _path_has_symlink_component(path: Path) -> bool:
    """Return whether any existing lexical component of ``path`` is a symlink."""
    absolute = path.absolute()
    current = Path(absolute.anchor)
    for component in absolute.parts[1:]:
        current /= component
        if current.is_symlink():
            return True
    return False


def _same_existing_file(left: Path, right: Path) -> bool:
    """Compare existing files by device/inode without trusting their spellings."""
    if not left.exists() or not right.exists():
        return False
    left_stat = left.stat()
    right_stat = right.stat()
    return (left_stat.st_dev, left_stat.st_ino) == (right_stat.st_dev, right_stat.st_ino)


def _sha256(path: Path) -> str:
    """Return the SHA-256 digest of a regular input file."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _expected_sha256(sidecar: Path) -> str:
    """Read the first SHA-256 token from an official sidecar file."""
    try:
        token = sidecar.read_text(encoding="utf-8").split()[0]
    except (FileNotFoundError, IndexError) as error:
        raise RoleContractError(f"missing official checksum sidecar: {sidecar}") from error
    if re.fullmatch(r"[0-9a-fA-F]{64}", token) is None:
        raise RoleContractError(f"invalid SHA-256 sidecar: {sidecar}")
    return token.lower()


@dataclass(frozen=True, slots=True)
class ImageRoles:
    """Resolved role manifest rooted at the repository containing ``os/``."""

    repository: Path
    manifest: Path
    values: dict[str, str]

    def _expand(self, name: str, active: frozenset[str] = frozenset()) -> str:
        """Expand the small, deterministic Make-variable subset used by roles."""
        if name in active:
            raise RoleContractError(f"recursive image-role manifest value: {name}")
        try:
            raw = self.values[name]
        except KeyError as error:
            raise RoleContractError(f"missing image-role manifest value: {name}") from error

        def replace(match: re.Match[str]) -> str:
            variable = match.group(1)
            if variable == "BUILD_ROOT":
                return str(self.repository / "build")
            if variable == "MODE":
                return os.environ.get("MODE", "release")
            return self._expand(variable, active | {name})

        return _MAKE_VARIABLE.sub(replace, raw)

    def path(self, name: str) -> Path:
        """Resolve a literal manifest path relative to ``os/make``."""
        raw = self._expand(name)
        return (self.repository / "os" / raw).resolve(strict=False)

    def official_x0(self, arch: str) -> Path:
        """Return the immutable x0 input for ``arch``."""
        return self.path(f"IMAGE_ROLE_{_ensure_arch(arch).upper()}_COMPETITION_X0")

    def derived_x0(self, arch: str) -> Path:
        """Return the only permitted mutable external-image derivative for ``arch``."""
        return self.path(f"IMAGE_ROLE_{_ensure_arch(arch).upper()}_DERIVED_X0")

    def derived_x0_next(self, arch: str) -> Path:
        """Return the staged next-round derivative for ``arch``."""
        return self.path(f"IMAGE_ROLE_{_ensure_arch(arch).upper()}_DERIVED_X0_NEXT")

    def official_archive(self, arch: str) -> Path:
        """Return the validated compressed evaluator input for ``arch``."""
        return self.path(f"IMAGE_ROLE_{_ensure_arch(arch).upper()}_COMPETITION_X0_ARCHIVE")

    def checksum_sidecar(self, arch: str, archive: bool) -> Path:
        """Return the manifest sidecar path for an official raw image or archive."""
        suffix = "ARCHIVE_CHECKSUM" if archive else "CHECKSUM"
        return self.path(f"IMAGE_ROLE_{_ensure_arch(arch).upper()}_COMPETITION_X0_{suffix}")

    def validate_official(self, arch: str, candidate: Path, *, archive: bool) -> Path:
        """Validate canonical identity and checksum before consuming official input."""
        expected = self.official_archive(arch) if archive else self.official_x0(arch)
        if _path_has_symlink_component(candidate):
            raise RoleContractError(f"official input must not use a symlink path: {candidate}")
        if candidate.resolve(strict=False) != expected.resolve(strict=False):
            raise RoleContractError(f"official input path does not match {arch} role: {candidate}")
        if not candidate.is_file():
            raise RoleContractError(f"official input is not a regular file: {candidate}")
        sidecar = self.checksum_sidecar(arch, archive)
        if sidecar.is_file():
            actual = _sha256(candidate)
            expected_digest = _expected_sha256(sidecar)
            if actual != expected_digest:
                raise RoleContractError(f"official input checksum mismatch: {candidate}")
        else:
            import sys
            print(f"image-role: checksum sidecar missing ({sidecar}), skipping integrity check", file=sys.stderr)
        return candidate.resolve(strict=True)

    def validate_derived_output(self, arch: str, candidate: Path, *, next_image: bool = False) -> Path:
        """Reject aliases before a caller can copy, fsck, or debugfs an output."""
        expected = self.derived_x0_next(arch) if next_image else self.derived_x0(arch)
        if _path_has_symlink_component(candidate):
            raise RoleContractError(f"derived output must not use a symlink path: {candidate}")
        if candidate.resolve(strict=False) != expected.resolve(strict=False):
            raise RoleContractError(f"derived output path does not match {arch} role: {candidate}")
        self.validate_mutable_output(candidate)
        return expected

    def validate_mutable_output(self, candidate: Path) -> Path:
        """Reject any symlink or official-x0 alias before a write operation."""
        if _path_has_symlink_component(candidate):
            raise RoleContractError(f"mutable output must not use a symlink path: {candidate}")
        if candidate.name in {self.official_x0(arch).name for arch in _ARCHES}:
            raise RoleContractError(f"mutable output uses an official x0 basename: {candidate}")
        for arch in sorted(_ARCHES):
            official = self.official_x0(arch)
            if candidate.resolve(strict=False) == official.resolve(strict=False) or _same_existing_file(candidate, official):
                raise RoleContractError(f"mutable output aliases immutable {arch} official x0: {candidate}")
        return candidate.resolve(strict=False)


def load_roles(repository: Path) -> ImageRoles:
    """Parse the Make role manifest without duplicating its values in Python."""
    root = repository.resolve(strict=True)
    manifest = root / "os/make/image-roles.mk"
    values: dict[str, str] = {}
    try:
        lines = manifest.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError as error:
        raise RoleContractError(f"missing image-role manifest: {manifest}") from error
    for line in lines:
        match = _ASSIGNMENT.match(line)
        if match is not None:
            values[match.group(1)] = match.group(2)
    if values.get("IMAGE_ROLE_MANIFEST_VERSION") != "2":
        raise RoleContractError("unsupported image-role manifest version")
    return ImageRoles(repository=root, manifest=manifest, values=values)


def _main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("official", "derived", "validate-official", "validate-derived", "validate-mutable"))
    parser.add_argument("--repo-root", required=True, type=Path)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--path", type=Path)
    parser.add_argument("--archive", action="store_true")
    parser.add_argument("--next", action="store_true")
    args = parser.parse_args()
    try:
        roles = load_roles(args.repo_root)
        match args.command:
            case "official":
                print(roles.official_archive(args.arch) if args.archive else roles.official_x0(args.arch))
            case "derived":
                print(roles.derived_x0(args.arch))
            case "validate-official":
                if args.path is None:
                    raise RoleContractError("--path is required for validate-official")
                print(roles.validate_official(args.arch, args.path, archive=args.archive))
            case "validate-derived":
                if args.path is None:
                    raise RoleContractError("--path is required for validate-derived")
                print(roles.validate_derived_output(args.arch, args.path, next_image=args.next))
            case "validate-mutable":
                if args.path is None:
                    raise RoleContractError("--path is required for validate-mutable")
                print(roles.validate_mutable_output(args.path))
    except RoleContractError as error:
        print(f"image-role error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
