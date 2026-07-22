"""Central QEMU command construction from the image-role manifest."""

from __future__ import annotations

import shlex
from dataclasses import dataclass
from pathlib import Path

from image_roles import ImageRoles


@dataclass(frozen=True, slots=True)
class QemuProfile:
    """An architecture-neutral QEMU launch profile."""

    name: str
    x0_role: str | None
    has_x1: bool
    kernel: str
    debug: bool = False


PROFILES: dict[str, QemuProfile] = {
    "normal": QemuProfile("normal", "derived", True, "competition"),
    "competition": QemuProfile("competition", "official", True, "competition"),
    "derived-competition": QemuProfile("derived-competition", "derived", True, "competition"),
    "development": QemuProfile("development", "development", True, "development"),
    "debug": QemuProfile("debug", "development", True, "development", debug=True),
    "regression": QemuProfile("regression", None, False, "regression"),
    "ktest": QemuProfile("ktest", None, False, "ktest"),
}


def _x0(roles: ImageRoles, arch: str, role: str) -> Path:
    match role:
        case "derived":
            return roles.derived_x0(arch)
        case "official":
            return roles.official_x0(arch)
        case "development":
            return roles.path(f"IMAGE_ROLE_{arch.upper()}_DEVELOPMENT_X0")
        case unreachable:
            raise AssertionError(f"unsupported x0 role: {unreachable}")


def _drives(roles: ImageRoles, arch: str, profile: QemuProfile) -> list[str]:
    if profile.x0_role is None:
        return []
    x0 = _x0(roles, arch, profile.x0_role)
    if arch == "rv64":
        device = ["-device", "virtio-blk-device,drive={drive},bus=virtio-mmio-bus.{bus}"]
    else:
        device = ["-device", "virtio-blk-pci,drive={drive}"]
    result = ["-drive", f"file={x0},if=none,format=raw,id=x0", device[0], device[1].format(drive="x0", bus=0)]
    if profile.has_x1:
        x1 = roles.path(f"IMAGE_ROLE_{arch.upper()}_X1")
        result.extend(["-drive", f"file={x1},if=none,format=raw,id=x1", device[0], device[1].format(drive="x1", bus=1)])
    return result


def build_qemu_args(roles: ImageRoles, arch: str, profile_name: str) -> tuple[str, ...]:
    """Return the canonical launch arguments for every supported QEMU profile."""
    profile = PROFILES[profile_name]
    if arch == "rv64":
        executable, kernel = "qemu-system-riscv64", "kernel-rv"
        network = ["-device", "virtio-net-device,netdev=net,bus=virtio-mmio-bus.7", "-netdev", "user,id=net"]
    else:
        executable, kernel = "qemu-system-loongarch64", "kernel-la"
        network = ["-device", "virtio-net-pci,netdev=net0", "-netdev", "user,id=net0"]
    args = [executable, "-machine", "virt", "-nographic"]
    if profile.kernel == "competition":
        args.extend(["-kernel", kernel, "-m", "1024", "-smp", "1"])
    else:
        args.extend(["-kernel", kernel, "-m", "1024", "-smp", "threads=1"])
    args.extend(_drives(roles, arch, profile))
    if profile.kernel == "competition":
        args.extend(["-no-reboot", "-rtc", "base=utc", *network])
    if profile.debug:
        args.extend(["-S", "-s"])
    return tuple(args)


def render_qemu_command(roles: ImageRoles, arch: str, profile_name: str) -> str:
    """Render a shell-safe canonical command for dry-run evidence."""
    return shlex.join(build_qemu_args(roles, arch, profile_name))


def build_rv64_cmd(roles: ImageRoles) -> str:
    """Retain the runner's RV64 normal-profile constructor."""
    return render_qemu_command(roles, "rv64", "normal")


def build_la64_cmd(roles: ImageRoles) -> str:
    """Retain the runner's LA64 normal-profile constructor."""
    return render_qemu_command(roles, "la64", "normal")
