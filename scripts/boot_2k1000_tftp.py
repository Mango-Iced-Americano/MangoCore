#!/usr/bin/env python3
"""One-click macOS network/TFTP/FTP/serial setup and 2K1000LA U-Boot boot."""

from __future__ import annotations

import argparse
import fcntl
import glob
import hashlib
import ipaddress
import os
import posixpath
from pathlib import Path
import re
import select
import shlex
import shutil
import subprocess
import sys
import termios
import time
import tty
from typing import Optional
from urllib.parse import quote
import zlib


DEFAULT_TFTP_ROOT = Path("/private/tftpboot")
TFTP_PLIST = Path("/System/Library/LaunchDaemons/tftp.plist")
TFTP_SERVICE = "system/com.apple.tftpd"
MONITOR_ESCAPE = 0x1D  # Ctrl-]
# 2K1000LA's shell RX path drops bursts while GMAC polling is active.  Keep
# automated shell commands at interactive typing speed, matching the board
# backup workflow; U-Boot commands remain burst-safe and are not affected.
BOARD_SHELL_INPUT_DELAY = 0.20
REPO_ROOT = Path(__file__).resolve().parents[1]
BUILD_LOCK_PATH = Path("/private/tmp/mango-2k1000-build.lock")
DEFAULT_FTP_PORT = 2121
DEFAULT_FTP_TIMEOUT = 600
SSD_P3_START_LBA = 0xA80800
SSD_TOOLS_ROOT = REPO_ROOT / "user" / "tools" / "loongarch64"
SSD_USER_BIN_DIR = (
    REPO_ROOT / "user" / "target" / "loongarch64-unknown-linux-gnu" / "release"
)
SSD_DISK_IMAGE = DEFAULT_TFTP_ROOT / "mango-2k1000la-full-test-mbr.img"
UIMAGE_HEADER_SIZE = 64
UIMAGE_MAGIC = 0x27051956
SAFE_TFTP_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")
# U-Boot may echo interrupt bytes immediately after the prompt (``=> ccc``)
# while the autoboot interrupter is still running.  Treat that bounded echo as
# a prompt too; acquire_prompt clears the raced input before issuing commands.
U_BOOT_PROMPT = re.compile(rb"(?:^|\r?\n)=>[ \t]*(?:c*)?(?:\r?\n|\r|$)")
SHELL_PROMPT = re.compile(
    rb"(?:^|\r?\n)[^\r\n]{0,256}/[^\r\n]*[#$][ \t]*(?:\r?\n|$)"
)
ANSI_ESCAPE = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
AUTOBOOT_MARKERS = (
    b"U-Boot",
    b"Autoboot",
    b"autoboot",
    b"Hit any key",
    b"Press any key",
)
SERIAL_DEVICE_GLOBS = (
    "/dev/cu.wchusbserial*",
    "/dev/tty.wchusbserial*",
    "/dev/cu.usbserial*",
    "/dev/tty.usbserial*",
    "/dev/cu.usbmodem*",
    "/dev/tty.usbmodem*",
)
BUILD_PROFILES = {
    "shell": (
        "GMAC Shell（网络调试）",
        "la64-2k1000-shell",
        "kernel-2k1000-shell.ui",
    ),
    "run": (
        "SATA 正式运行镜像",
        "la64-2k1000-run-clean",
        "kernel-2k1000-run.ui",
    ),
    "mainline": (
        "SATA 持久根主线镜像",
        "la64-2k1000-mainline",
        "kernel-2k1000-mainline.ui",
    ),
    "core-tests": (
        "SATA 核心测试镜像",
        "la64-2k1000-core-tests",
        "kernel-2k1000-core-tests.ui",
    ),
    "apk-persist": (
        "APK 持久化 Shell",
        "la64-2k1000-apk-persist-shell",
        "kernel-2k1000-persist-shell.ui",
    ),
}


class BootError(RuntimeError):
    pass


def normalize_ipv4(value: str, label: str) -> str:
    try:
        address = ipaddress.IPv4Address(value)
    except ipaddress.AddressValueError as error:
        raise BootError(f"{label} must be an IPv4 address: {value!r}") from error
    if (
        address.is_unspecified
        or address.is_multicast
        or address.is_loopback
        or address.is_reserved
        or address.is_link_local
    ):
        raise BootError(f"{label} is not a usable board-link IPv4 address: {value!r}")
    return str(address)


def normalize_netmask(value: str) -> str:
    try:
        address = ipaddress.IPv4Address(value)
    except ipaddress.AddressValueError as error:
        raise BootError(f"netmask must be a dotted IPv4 netmask: {value!r}") from error
    bits = f"{int(address):032b}"
    if "01" in bits:
        raise BootError(f"netmask is not contiguous: {value!r}")
    return str(address)


def validate_cli_inputs(args: argparse.Namespace) -> None:
    args.host_ip = normalize_ipv4(args.host_ip, "host_ip")
    args.board_ip = normalize_ipv4(args.board_ip, "board_ip")
    if args.host_ip == args.board_ip:
        raise BootError("host_ip and board_ip must be different")
    args.netmask = normalize_netmask(args.netmask)

    try:
        loadaddr = int(args.loadaddr, 0)
    except ValueError as error:
        raise BootError(f"loadaddr must be an integer address: {args.loadaddr!r}") from error
    if not 0 <= loadaddr <= 0xFFFFFFFFFFFFFFFF or loadaddr % 4:
        raise BootError(f"loadaddr must be a 4-byte-aligned 64-bit address: {args.loadaddr!r}")
    args.loadaddr = f"0x{loadaddr:x}"

    if args.reset_timeout <= 0 or args.tftp_timeout <= 0:
        raise BootError("reset and TFTP timeouts must be positive")
    if args.baud <= 0:
        raise BootError("baud must be positive")
    if not 1 <= args.ftp_port <= 65535:
        raise BootError("FTP port must be between 1 and 65535")
    if args.ftp_timeout <= 0:
        raise BootError("FTP timeout must be positive")
    if args.ftp_destination is not None and args.ftp_upload is None:
        raise BootError("--ftp-destination requires --ftp-upload")
    if args.ftp_board_ip is not None and args.ftp_upload is None:
        raise BootError("--ftp-board-ip requires --ftp-upload")
    if args.ftp_upload is not None:
        source = args.ftp_upload.expanduser().resolve()
        if not source.is_file():
            raise BootError(f"FTP source file not found: {source}")
        args.ftp_upload = source
        args.ftp_destination = validate_ftp_destination(
            args.ftp_destination, source.name
        )
        if args.ftp_board_ip is not None:
            args.ftp_board_ip = normalize_ipv4(args.ftp_board_ip, "ftp_board_ip")
            if args.ftp_board_ip == args.host_ip:
                raise BootError("ftp_board_ip and host_ip must be different")


def validate_tftp_root(tftp_root: Path) -> Path:
    root = tftp_root.expanduser().resolve()
    expected = DEFAULT_TFTP_ROOT.resolve()
    if root != expected:
        raise BootError(
            f"macOS system tftpd serves only {expected}; custom --tftp-root {root} "
            "is rejected because copying there would not affect the board transfer"
        )
    return root


def validate_tftp_filename(name: str) -> None:
    if SAFE_TFTP_NAME.fullmatch(name) is None:
        raise BootError(
            f"unsafe TFTP filename {name!r}; use only ASCII letters, digits, '.', '_' and '-'")


def validate_ftp_destination(value: Optional[str], source_name: str) -> str:
    destination = value or f"/persist/{source_name}"
    if not destination.startswith("/persist/"):
        raise BootError(
            "--ftp-destination must be an absolute path below /persist (the writable SSD P4)"
        )
    if any(character in destination for character in "\0\r\n"):
        raise BootError("--ftp-destination contains a NUL or line break")
    try:
        destination.encode("ascii")
    except UnicodeEncodeError as error:
        raise BootError("--ftp-destination must contain ASCII characters") from error
    normalized = posixpath.normpath(destination)
    if (
        normalized != destination
        or normalized == "/persist"
        or not normalized.startswith("/persist/")
        or any(part in {"", ".", ".."} for part in normalized.split("/")[1:])
    ):
        raise BootError(
            "--ftp-destination must name one file below /persist without '.', '..' or duplicate separators"
        )
    return normalized


def http_server_command(directory: Path, host: str, port: int) -> list[str]:
    """Serve one host directory for the board-side BusyBox wget pull."""
    ruby = Path("/usr/bin/ruby")
    if sys.platform == "darwin" and ruby.is_file():
        return [str(ruby), "-run", "-e", "httpd", str(directory), "-p", str(port), "-b", host]
    return [
        sys.executable,
        "-m",
        "http.server",
        str(port),
        "--bind",
        host,
        "--directory",
        str(directory),
    ]


def validate_uimage(path: Path) -> None:
    try:
        size = path.stat().st_size
        with path.open("rb") as source:
            header = source.read(UIMAGE_HEADER_SIZE)
    except OSError as error:
        raise BootError(f"cannot read uImage {path}: {error}") from error
    if len(header) != UIMAGE_HEADER_SIZE:
        raise BootError(f"uImage header is truncated: {path}")
    magic = int.from_bytes(header[:4], "big")
    if magic != UIMAGE_MAGIC:
        raise BootError(f"invalid U-Boot legacy image magic in {path}: {magic:#x}")
    expected_header_crc = int.from_bytes(header[4:8], "big")
    header_for_crc = bytearray(header)
    header_for_crc[4:8] = b"\0\0\0\0"
    actual_header_crc = zlib.crc32(header_for_crc)
    if actual_header_crc != expected_header_crc:
        raise BootError(
            f"uImage header CRC mismatch for {path}: "
            f"{actual_header_crc:08x} != {expected_header_crc:08x}"
        )
    payload_size = int.from_bytes(header[12:16], "big")
    if payload_size != size - UIMAGE_HEADER_SIZE:
        raise BootError(
            f"uImage length mismatch for {path}: header={payload_size}, "
            f"file={size - UIMAGE_HEADER_SIZE}"
        )


def prompt_seen(data: bytes) -> bool:
    # The board clears the console with an ANSI sequence immediately before
    # printing the prompt; strip it so ``\x1b[2J=> c`` is recognized too.
    return U_BOOT_PROMPT.search(ANSI_ESCAPE.sub(b"", data)) is not None


def autoboot_seen(data: bytes) -> bool:
    return any(marker in data for marker in AUTOBOOT_MARKERS)


def build_lock():
    BUILD_LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    lock = BUILD_LOCK_PATH.open("a+")
    fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
    return lock


def run(
    command: list[str],
    *,
    check: bool = True,
    cwd: Optional[Path] = None,
    env: Optional[dict[str, str]] = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        check=check,
        cwd=str(cwd) if cwd is not None else None,
        env=env,
        text=True,
        capture_output=True,
    )


def run_streaming(
    command: list[str], *, cwd: Path, env: Optional[dict[str, str]] = None
) -> None:
    print("+", shlex.join(command), flush=True)
    subprocess.run(command, cwd=str(cwd), env=env, check=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            digest.update(data)
    return digest.hexdigest()


def crc32(path: Path) -> int:
    checksum = 0
    with path.open("rb") as source:
        while data := source.read(4 * 1024 * 1024):
            checksum = zlib.crc32(data, checksum)
    return checksum


def sudo(command: list[str]) -> None:
    print("+ sudo", " ".join(command), flush=True)
    subprocess.run(["sudo", *command], check=True)


def interface_netmask(output: str, host_ip: str) -> Optional[str]:
    match = re.search(
        rf"^\s*inet\s+{re.escape(host_ip)}\s+netmask\s+(\S+)",
        output,
        re.MULTILINE,
    )
    if match is None:
        return None
    value = match.group(1)
    try:
        if value.lower().startswith("0x"):
            value = str(ipaddress.IPv4Address(int(value, 16)))
        return normalize_netmask(value)
    except (ValueError, BootError):
        return None


def ensure_interface(interface: str, host_ip: str, netmask: str, configure: bool) -> None:
    host_ip = normalize_ipv4(host_ip, "host_ip")
    netmask = normalize_netmask(netmask)
    result = run(["ifconfig", interface], check=False)
    if result.returncode != 0:
        raise BootError(f"network interface not found: {interface}")
    current = result.stdout
    current_mask = interface_netmask(current, host_ip)
    if current_mask == netmask:
        print(f"[host] {interface} already uses {host_ip}/{netmask}")
    elif configure:
        sudo(["ifconfig", interface, "inet", host_ip, "netmask", netmask, "up"])
        current = run(["ifconfig", interface]).stdout
        if interface_netmask(current, host_ip) != netmask:
            raise BootError(f"failed to configure {host_ip}/{netmask} on {interface}")
        print(f"[host] configured {interface} as {host_ip}/{netmask}")
    else:
        if current_mask is None:
            raise BootError(
                f"{interface} does not have {host_ip}/{netmask}; "
                "rerun without --no-host-config"
            )
        raise BootError(
            f"{interface} has {host_ip}/{current_mask}, expected /{netmask}; "
            "rerun without --no-host-config"
        )
    if "status: active" not in current:
        raise BootError(f"{interface} has no active Ethernet link")


def ensure_tftp_service(configure: bool, tftp_root: Path = DEFAULT_TFTP_ROOT) -> None:
    tftp_root = validate_tftp_root(tftp_root)
    if not TFTP_PLIST.is_file():
        raise BootError(f"TFTP launch daemon plist not found: {TFTP_PLIST}")
    try:
        plist = TFTP_PLIST.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        raise BootError(f"cannot read TFTP launch daemon plist: {error}") from error
    if str(tftp_root) not in plist:
        raise BootError(
            f"TFTP plist does not advertise the expected service root {tftp_root}; "
            "refusing to publish an image that the board cannot fetch"
        )
    def service_status() -> Optional[str]:
        result = run(["launchctl", "print", TFTP_SERVICE], check=False)
        if result.returncode != 0:
            return None
        required = (
            f"{tftp_root}",
            "type = datagram",
            "service name = tftp",
            "passive = 1",
        )
        missing = [entry for entry in required if entry not in result.stdout]
        if missing:
            raise BootError(
                f"loaded TFTP service is missing its UDP/69 contract: {', '.join(missing)}"
            )
        return result.stdout

    if service_status() is not None:
        print(f"[host] TFTP service ready: {TFTP_SERVICE} (UDP/69, root={tftp_root})")
        return
    if not configure:
        raise BootError("macOS TFTP service is not loaded")
    sudo(["launchctl", "load", "-F", str(TFTP_PLIST)])
    if service_status() is None:
        raise BootError("failed to load the macOS TFTP service")
    print(f"[host] loaded TFTP service: {TFTP_SERVICE} (UDP/69, root={tftp_root})")


def prepare_tftp_image(
    image: Path, tftp_root: Path, *, require_uimage: bool = False
) -> Path:
    image = image.expanduser().resolve()
    if not image.is_file():
        raise BootError(f"kernel image not found: {image}")
    validate_tftp_filename(image.name)
    if image.stat().st_size == 0:
        raise BootError(f"TFTP image is empty: {image}")
    if require_uimage:
        validate_uimage(image)

    tftp_root = tftp_root.expanduser().resolve()
    if not tftp_root.exists():
        try:
            tftp_root.mkdir(parents=True)
        except PermissionError:
            sudo(["mkdir", "-p", str(tftp_root)])
    destination = tftp_root / image.name
    if destination.is_symlink():
        raise BootError(f"refusing to publish through a symlink: {destination}")
    source_hash = sha256(image)
    if destination != image and (
        not destination.is_file() or sha256(destination) != source_hash
    ):
        temporary = destination.with_name(
            f".{destination.name}.mango-tmp-{os.getpid()}"
        )
        try:
            shutil.copy2(image, temporary)
            os.replace(temporary, destination)
        except PermissionError:
            # /private/tftpboot is normally root:wheel. Use a same-directory
            # temporary name and sudo mv so tftpd never observes a partial file.
            sudo(["cp", "-f", str(image), str(temporary)])
            sudo(["mv", "-f", str(temporary), str(destination)])
        finally:
            if temporary.exists():
                try:
                    temporary.unlink()
                except PermissionError:
                    sudo(["rm", "-f", str(temporary)])
    if not destination.is_file() or sha256(destination) != source_hash:
        raise BootError("TFTP image copy verification failed")
    if destination.stat().st_mode & 0o777 != 0o644:
        try:
            destination.chmod(0o644)
        except PermissionError:
            sudo(["chmod", "0644", str(destination)])
    print(
        f"[host] TFTP image ready: {destination} "
        f"({destination.stat().st_size} bytes, sha256={source_hash})"
    )
    return destination


def serial_candidates() -> list[str]:
    """Return supported macOS USB-UART paths without hiding unpaired devices."""
    candidates = sorted(
        {
            path
            for pattern in SERIAL_DEVICE_GLOBS
            for path in glob.glob(pattern)
        }
    )
    callout = [path for path in candidates if path.startswith("/dev/cu.")]
    callout_keys = {path.split("/dev/cu.", 1)[1] for path in callout}
    unpaired_tty = [
        path
        for path in candidates
        if path.startswith("/dev/tty.")
        and path.split("/dev/tty.", 1)[1] not in callout_keys
    ]
    return sorted(callout + unpaired_tty) if callout else candidates


def detect_serial(explicit: Optional[str]) -> str:
    if explicit:
        path = str(Path(explicit).expanduser())
        candidate = Path(path)
        if not candidate.exists() or not candidate.is_char_device():
            raise BootError(f"serial device not found: {path}")
        return path
    candidates = serial_candidates()
    if len(candidates) != 1:
        raise BootError(
            "expected exactly one supported USB serial device; "
            f"found {candidates or 'none'} (use --serial)"
        )
    return candidates[0]


def interface_candidates() -> list[str]:
    """Return host interfaces reported by macOS without changing state."""
    result = run(["ifconfig", "-l"], check=False)
    if result.returncode != 0:
        return []
    return result.stdout.split()


def build_profile_choices() -> list[tuple[str, str]]:
    return [(key, value[0]) for key, value in BUILD_PROFILES.items()]


def build_image_path(profile: str, build_mode: str) -> Path:
    image_name = BUILD_PROFILES[profile][2]
    return REPO_ROOT / "build" / "la64" / build_mode / "normal" / "board" / "2k1000" / image_name


def docker_compose_env() -> dict[str, str]:
    """Pass host proxies to Docker using host.docker.internal instead of localhost."""
    env = os.environ.copy()

    def translate_proxy(value: str) -> str:
        match = re.match(
            r"^(?P<scheme>[A-Za-z][A-Za-z0-9+.-]*://)"
            r"(?P<auth>[^/@]+@)?"
            r"(?P<host>127\.0\.0\.1|localhost|\[::1\])"
            r"(?P<suffix>[:/].*)?$",
            value,
        )
        if match is None:
            return value
        return (
            f"{match.group('scheme')}{match.group('auth') or ''}"
            f"host.docker.internal{match.group('suffix') or ''}"
        )

    for name in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
        override = env.get(f"MANGO_DOCKER_{name}")
        source = override if override is not None else env.get(name) or env.get(name.lower(), "")
        env[f"MANGO_DOCKER_{name}"] = translate_proxy(source)

    no_proxy = env.get("NO_PROXY") or env.get("no_proxy", "")
    if "host.docker.internal" not in no_proxy:
        no_proxy = f"{no_proxy},host.docker.internal" if no_proxy else "host.docker.internal"
    env["MANGO_DOCKER_NO_PROXY"] = no_proxy
    return env


def ensure_docker_service() -> dict[str, str]:
    if shutil.which("docker") is None:
        raise BootError("未找到 docker；请启动 Docker Desktop")
    compose_env = docker_compose_env()
    status = run(
        ["docker", "compose", "ps", "--status", "running", "--services"],
        cwd=REPO_ROOT,
        check=False,
        env=compose_env,
    )
    if "os-dev" not in status.stdout.split():
        print("[build] 启动 Docker os-dev 容器…", flush=True)
    run_streaming(
        ["docker", "compose", "up", "-d", "os-dev"],
        cwd=REPO_ROOT,
        env=compose_env,
    )
    return compose_env


def build_image(profile: str, build_mode: str) -> Path:
    if profile not in BUILD_PROFILES:
        raise BootError(f"unknown 2K1000LA build profile: {profile}")
    label, target, _ = BUILD_PROFILES[profile]
    output = build_image_path(profile, build_mode)
    print(f"[build] Docker/{label}: {target}, MODE={build_mode}", flush=True)

    make_args = ["make", "-C", "os", "-B", target, f"MODE={build_mode}"]
    build_started = time.time()
    lock = build_lock()
    try:
        compose_env = ensure_docker_service()
        run_streaming(
            [
                "docker",
                "compose",
                "exec",
                "-T",
                "-w",
                "/app",
                "os-dev",
                "make",
                "-C",
                "os",
                "toolchain-setup",
            ],
            cwd=REPO_ROOT,
            env=compose_env,
        )
        run_streaming(
            [
                "docker",
                "compose",
                "exec",
                "-T",
                "-w",
                "/app",
                "os-dev",
                *make_args,
            ],
            cwd=REPO_ROOT,
            env=compose_env,
        )
    finally:
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        lock.close()

    if not output.is_file():
        raise BootError(f"build completed but image was not found: {output}")
    if output.stat().st_mtime < build_started - 5:
        raise BootError(f"build left a stale image untouched: {output}")
    validate_uimage(output)
    print(f"[build] image ready: {output} ({output.stat().st_size} bytes)", flush=True)
    return output


def container_repo_path(path: Path, label: str) -> str:
    resolved = path.expanduser().resolve()
    try:
        relative = resolved.relative_to(REPO_ROOT)
    except ValueError as error:
        raise BootError(
            f"{label} must be inside {REPO_ROOT} so Docker can access it: {resolved}"
        ) from error
    return f"/app/{relative}"


def validate_ssd_install_gate(args: argparse.Namespace) -> None:
    try:
        confirmed_start = int(args.confirm_ssd_p3_start, 0)
    except (TypeError, ValueError) as error:
        raise BootError(
            f"--confirm-ssd-p3-start must equal {SSD_P3_START_LBA:#x}"
        ) from error
    if confirmed_start != SSD_P3_START_LBA:
        raise BootError(
            f"SSD P3 confirmation mismatch: {confirmed_start:#x} != "
            f"{SSD_P3_START_LBA:#x}"
        )
    if not args.ssd_backup_id:
        raise BootError(
            "--ssd-backup-id is required; create a complete P3 backup in P4 first"
        )
    if (
        re.fullmatch(r"[A-Za-z0-9._-]+", args.ssd_backup_id) is None
        or args.ssd_backup_id in {".", ".."}
    ):
        raise BootError("--ssd-backup-id contains unsafe characters")
    if not args.ssd_disk_image.expanduser().is_file():
        raise BootError(f"SSD reference disk image not found: {args.ssd_disk_image}")


def install_ssd_kernel(args: argparse.Namespace) -> None:
    """Rebuild and safely replace P3 with the selected kernel in slot A."""
    validate_ssd_install_gate(args)

    kernel = args.image.expanduser().resolve()
    validate_uimage(kernel)
    for label, path in (
        ("tools root", SSD_TOOLS_ROOT),
        ("user binary directory", SSD_USER_BIN_DIR),
    ):
        if not path.is_dir():
            raise BootError(f"{label} not found: {path}")

    p3_image = (
        args.ssd_p3_image.expanduser().resolve()
        if args.ssd_p3_image is not None
        else build_image_path("mainline", args.build_mode).with_name(
            "mango-mainline-root-p3.img"
        )
    )
    manifest = Path(f"{p3_image}.json")
    container_tools_root = container_repo_path(SSD_TOOLS_ROOT, "tools root")
    container_user_bin = container_repo_path(
        SSD_USER_BIN_DIR, "user binary directory"
    )
    container_kernel = container_repo_path(kernel, "kernel image")
    container_p3 = container_repo_path(p3_image, "P3 output")
    compose_env = ensure_docker_service()
    lock = build_lock()
    try:
        run_streaming(
            [
                "docker",
                "compose",
                "exec",
                "-T",
                "-w",
                "/app",
                "os-dev",
                "python3",
                "scripts/make_2k1000_tools_partition.py",
                "--tools-root",
                container_tools_root,
                "--user-bin-dir",
                container_user_bin,
                "--kernel-image",
                container_kernel,
                "--output",
                container_p3,
                "--force",
            ],
            cwd=REPO_ROOT,
            env=compose_env,
        )
    finally:
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        lock.close()

    command = [
        sys.executable,
        str(REPO_ROOT / "scripts" / "write_2k1000_p3.py"),
        "--interface",
        args.interface,
        "--tftp-root",
        str(args.tftp_root),
        "--image",
        str(p3_image),
        "--manifest",
        str(manifest),
        "--verify-kernel",
        str(kernel),
        "--disk-image",
        str(args.ssd_disk_image),
        "--host-ip",
        args.host_ip,
        "--board-ip",
        args.board_ip,
        "--netmask",
        args.netmask,
        "--reset-timeout",
        str(args.reset_timeout),
        "--tftp-timeout",
        str(args.tftp_timeout),
        "--confirm-p3-start",
        f"{SSD_P3_START_LBA:#x}",
        "--backup-id",
        args.ssd_backup_id,
    ]
    if args.serial:
        command.extend(("--serial", args.serial))
    if not args.configure_host:
        command.append("--no-host-config")
    run_streaming(command, cwd=REPO_ROOT)
    print(
        f"[ssd] PASS: {kernel.name} installed as P3 /boot/kernel-A.ui; "
        "existing U-Boot local-boot configuration will load it on the next reset",
        flush=True,
    )


def image_candidates() -> list[Path]:
    """Find likely 2K1000 images for the interactive picker."""
    roots = (REPO_ROOT, Path.cwd(), REPO_ROOT / "build", Path.cwd() / "build")
    unique_roots: list[Path] = []
    for root in roots:
        root = root.resolve()
        if root not in unique_roots:
            unique_roots.append(root)
    matches: set[Path] = set()
    for root in unique_roots:
        if not root.is_dir():
            continue
        if root in {REPO_ROOT, Path.cwd().resolve()}:
            iterator = root.glob("kernel-2k1000*.ui")
        else:
            iterator = root.rglob("kernel-2k1000*.ui")
        matches.update(path.resolve() for path in iterator if path.is_file())
    return sorted(matches, key=lambda path: path.stat().st_mtime, reverse=True)


def prompt_text(label: str, default: Optional[str] = None) -> str:
    suffix = f" [{default}]" if default is not None else ""
    while True:
        try:
            value = input(f"{label}{suffix}: ").strip()
        except EOFError as error:
            raise BootError(
                "interactive input ended; rerun with --non-interactive"
            ) from error
        if value:
            return value
        if default is not None:
            return default
        print("请输入有效值。")


def prompt_yes_no(label: str, default: bool) -> bool:
    default_text = "Y/n" if default else "y/N"
    while True:
        try:
            value = input(f"{label} [{default_text}]: ").strip().lower()
        except EOFError as error:
            raise BootError(
                "interactive input ended; rerun with --non-interactive"
            ) from error
        if not value:
            return default
        if value in {"y", "yes", "是"}:
            return True
        if value in {"n", "no", "否"}:
            return False
        print("请输入 y 或 n。")


def prompt_choice(label: str, choices: list[tuple[str, str]], default: str) -> str:
    rendered = "/".join(f"{key}={description}" for key, description in choices)
    valid = {key for key, _ in choices}
    while True:
        value = prompt_text(f"{label} ({rendered})", default)
        if value in valid:
            return value
        if value.isdigit():
            index = int(value) - 1
            if 0 <= index < len(choices):
                return choices[index][0]
        print(f"请输入选项编号或：{', '.join(sorted(valid))}。")


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(Path.cwd()))
    except ValueError:
        return str(path)


def prompt_image(default: Optional[Path]) -> Path:
    if default is not None:
        return Path(prompt_text("镜像路径", str(default))).expanduser()

    candidates = image_candidates()
    if candidates:
        print("\n发现以下候选 uImage：")
        for index, path in enumerate(candidates, 1):
            print(f"  {index}. {display_path(path)}")
        print("  p. 手动输入路径")
        selection = prompt_text("选择镜像编号或输入路径", "1")
        if selection.lower() != "p" and selection.isdigit():
            index = int(selection) - 1
            if 0 <= index < len(candidates):
                return candidates[index]
        elif selection.lower() != "p":
            return Path(selection).expanduser()
    return Path(prompt_text("镜像路径")).expanduser()


def prompt_serial(default: Optional[str]) -> str:
    if default:
        return prompt_text("串口设备", default)

    candidates = serial_candidates()
    if len(candidates) == 1:
        candidate = candidates[0]
        if prompt_yes_no(f"检测到串口 {candidate}，使用它", True):
            return candidate
    elif candidates:
        print("\n检测到多个串口：")
        for index, path in enumerate(candidates, 1):
            print(f"  {index}. {path}")
        selection = prompt_text("选择串口编号或输入路径", "1")
        if selection.isdigit() and 0 < int(selection) <= len(candidates):
            return candidates[int(selection) - 1]
        return selection
    else:
        print("未自动检测到 USB 串口。")
    return prompt_text("串口设备路径")


def interactive_config(args: argparse.Namespace) -> argparse.Namespace:
    print("\n=== MangoCore 2K1000LA 一键上板向导 ===")
    print("默认流程：编译镜像 → 配置网络 → 发布 TFTP → 检测串口 → U-Boot 校验。\n")

    default_mode = (
        "monitor"
        if args.monitor_only
        else "ssd-install"
        if args.install_ssd_kernel
        else "upload-only"
        if args.upload_only
        else "ftp-upload"
        if args.ftp_upload is not None
        else "prepare"
        if args.prepare_only
        else "transfer"
        if args.no_boot
        else "boot"
    )
    mode = prompt_choice(
        "执行模式",
        [
            ("boot", "传输并启动"),
            ("monitor", "仅打开串口（SSD 自动启动，可交互）"),
            ("ftp-upload", "启动后通过 FTP 写入 SSD"),
            ("upload-only", "连接已运行系统并直接上传文件"),
            ("transfer", "只传输并校验"),
            ("prepare", "只准备主机"),
            ("ssd-install", "备份门禁后写入 SSD A 槽"),
        ],
        default_mode,
    )
    args.monitor_only = mode == "monitor"
    args.prepare_only = mode == "prepare"
    args.no_boot = mode == "transfer"
    args.install_ssd_kernel = mode == "ssd-install"
    args.upload_only = mode == "upload-only"
    if mode not in {"ftp-upload", "upload-only"} and any(
        (
            args.ftp_upload is not None,
            args.ftp_destination is not None,
            args.ftp_board_ip is not None,
        )
    ):
        raise BootError("FTP options require the interactive ftp-upload mode")

    if args.monitor_only:
        args.build_profile = None
        args.image = None
        args.serial = prompt_serial(args.serial)
        args.takeover_screen = prompt_yes_no(
            "若串口被 screen 占用，自动接管", args.takeover_screen
        )
        print("\n配置摘要：")
        print("  编译/网络/TFTP：跳过")
        print(f"  串口：{args.serial}")
        print("  模式：monitor（不自动发命令，手工输入正常转发）")
        if not prompt_yes_no("确认执行", True):
            raise BootError("用户取消操作")
        return args

    if mode == "upload-only":
        args.build_profile = None
        args.image = None
        available = interface_candidates()
        if available:
            print(f"可用网卡：{', '.join(available)}")
        args.interface = prompt_text("宿主机网卡", args.interface)
        args.host_ip = prompt_text("宿主机 IP", args.host_ip)
        args.board_ip = prompt_text("开发板附加 IP", args.board_ip)
        args.netmask = prompt_text("子网掩码", args.netmask)
        args.configure_host = prompt_yes_no(
            "自动配置宿主机网卡（HTTP 上传需要）", args.configure_host
        )
        args.serial = prompt_serial(args.serial)
        if args.ftp_upload is None:
            args.ftp_upload = Path(prompt_text("要传输的本地文件")).expanduser()
        args.ftp_destination = prompt_text(
            "SSD 目标路径（必须位于 /persist）",
            args.ftp_destination or f"/persist/{args.ftp_upload.name}",
        )
        args.takeover_screen = prompt_yes_no(
            "若串口被 screen 占用，自动接管", args.takeover_screen
        )
        print("\n配置摘要：")
        print("  编译：跳过")
        print("  TFTP/复位/启动：跳过")
        print(f"  文件：{args.ftp_upload} -> {args.ftp_destination}")
        print(f"  串口：{args.serial}")
        print("  模式：upload-only（板子必须已经在 P3 shell）")
        if not prompt_yes_no("确认执行", True):
            raise BootError("用户取消操作")
        return args

    source_default = "build" if args.build_profile or args.image is None else "existing"
    source = prompt_choice(
        "镜像来源",
        [("build", "编译新镜像"), ("existing", "使用已有镜像")],
        source_default,
    )
    if source == "build":
        args.build_profile = prompt_choice(
            "编译模式", build_profile_choices(), args.build_profile or "mainline"
        )
        args.build_mode = prompt_choice(
            "编译配置",
            [("release", "Release"), ("debug", "Debug")],
            args.build_mode,
        )
        args.image = build_image_path(args.build_profile, args.build_mode)
    else:
        args.build_profile = None
        args.image = prompt_image(args.image)

    available = interface_candidates()
    if available:
        print(f"可用网卡：{', '.join(available)}")
    args.interface = prompt_text("宿主机网卡", args.interface)
    args.host_ip = prompt_text("宿主机 IP", args.host_ip)
    args.board_ip = prompt_text("开发板 IP", args.board_ip)
    args.netmask = prompt_text("子网掩码", args.netmask)
    args.configure_host = prompt_yes_no(
        "自动配置网卡并启动 macOS TFTP 服务", args.configure_host
    )
    args.serial = prompt_serial(args.serial)

    if args.install_ssd_kernel:
        print("警告：该模式会重建并覆盖 SSD 的完整 768 MiB P3 根分区。")
        args.ssd_backup_id = prompt_text("P4 中已完成的 P3 备份 ID")
        args.confirm_ssd_p3_start = prompt_text(
            f"输入固定 P3 起始 LBA {SSD_P3_START_LBA:#x} 以确认覆盖"
        )
    if mode == "ftp-upload":
        if args.ftp_upload is None:
            args.ftp_upload = Path(prompt_text("要传输的本地文件")).expanduser()
        args.ftp_destination = prompt_text(
            "SSD 目标路径（必须位于 /persist）",
            args.ftp_destination or f"/persist/{args.ftp_upload.name}",
        )
    if not args.prepare_only:
        args.takeover_screen = prompt_yes_no(
            "若串口被 screen 占用，自动接管", args.takeover_screen
        )

    print("\n配置摘要：")
    if args.build_profile:
        label, target, _ = BUILD_PROFILES[args.build_profile]
        print(f"  编译：Docker/{label} ({target}, MODE={args.build_mode})")
    else:
        print("  编译：跳过，使用已有镜像")
    print(f"  镜像：{args.image}")
    print(f"  网卡：{args.interface} {args.host_ip}/{args.netmask}")
    print(f"  开发板：{args.board_ip}")
    print(f"  串口：{args.serial}")
    print(f"  模式：{mode}")
    if mode == "ftp-upload":
        print(f"  FTP 文件：{args.ftp_upload} -> {args.ftp_destination}")
    if not prompt_yes_no("确认执行", True):
        raise BootError("用户取消操作")
    return args


def release_matching_screen(serial_path: str, takeover: bool) -> None:
    processes = run(["ps", "-ax", "-o", "pid=,command="], check=False).stdout
    owners: list[str] = []
    for line in processes.splitlines():
        if serial_path not in line or re.search(
            r"(?:^|\s)(?:\S*/)?screen(?:\s|$)", line, re.IGNORECASE
        ) is None:
            continue
        fields = line.strip().split(None, 1)
        if fields:
            owners.append(fields[0])
    if not owners:
        return
    if not takeover:
        raise BootError(
            f"serial device is held by screen PID(s) {owners}; close screen or allow takeover"
        )
    sessions = run(["screen", "-ls"], check=False).stdout
    for pid in owners:
        match = re.search(rf"^\s*({re.escape(pid)}\.[^\s]+)", sessions, re.MULTILINE)
        if match is None:
            raise BootError(f"could not resolve screen session for PID {pid}")
        session = match.group(1)
        print(f"[host] closing screen session that owns serial: {session}")
        subprocess.run(["screen", "-S", session, "-X", "quit"], check=True)
    time.sleep(0.5)


class UBootConsole:
    def __init__(self, serial_module, port: str, log_path: Path, baud: int = 115200) -> None:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        self.log = log_path.open("ab", buffering=0)
        self.last_read = b""
        try:
            self.serial = serial_module.Serial(
                port,
                baud,
                timeout=0.2,
                write_timeout=2,
                exclusive=True,
            )
        except serial_module.SerialException as error:
            self.log.close()
            raise BootError(f"cannot open serial device {port}: {error}") from error
        self.serial.dtr = False
        self.serial.rts = False

    def close(self) -> None:
        self.serial.close()
        self.log.close()

    def _write_console_input(self, data: bytes) -> None:
        # Interactive typing usually arrives one byte at a time.  The 2K1000
        # kernel TTY can drop bytes even from short bursts while GMAC polling
        # is active, so automated commands must reproduce the board's reliable
        # interactive rate instead of using a host-terminal paste rate.
        chunk_size = 1
        for offset in range(0, len(data), chunk_size):
            self.serial.write(data[offset : offset + chunk_size])
            self.serial.flush()
            if offset + chunk_size < len(data):
                time.sleep(BOARD_SHELL_INPUT_DELAY)

    def _handle_console_input(
        self, data: bytes, escape_pending: bool
    ) -> tuple[bool, bool]:
        """Forward terminal bytes and reserve Ctrl-] commands for the monitor."""
        forward = bytearray()
        for byte in data:
            if escape_pending:
                escape_pending = False
                if byte == ord("q"):
                    if forward:
                        self._write_console_input(bytes(forward))
                    return True, False
                if byte == ord("c"):
                    forward.append(0x03)
                elif byte == MONITOR_ESCAPE:
                    forward.append(MONITOR_ESCAPE)
                elif byte == ord("?"):
                    print(
                        "\r\n[console] Ctrl-C sends SIGINT to the board; "
                        "Ctrl-] q closes this monitor; Ctrl-] Ctrl-] sends Ctrl-]",
                        flush=True,
                    )
                else:
                    # Unknown escape commands are passed through literally so
                    # the monitor does not silently discard terminal input.
                    forward.extend((MONITOR_ESCAPE, byte))
                continue

            if byte == MONITOR_ESCAPE:
                escape_pending = True
            else:
                forward.append(byte)

        if forward:
            self._write_console_input(bytes(forward))
        return False, escape_pending

    def _record(self, data: bytes) -> None:
        self.log.write(data)

    def _read_prompt(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        output = bytearray()
        while time.monotonic() < deadline:
            data = self.serial.read(self.serial.in_waiting or 1)
            if not data:
                continue
            output.extend(data)
            self._record(data)
            if prompt_seen(output):
                self.last_read = bytes(output)
                return output.decode("utf-8", errors="replace")
        self.last_read = bytes(output)
        tail = output[-1000:].decode("utf-8", errors="replace")
        raise BootError(f"timeout waiting for U-Boot prompt; tail:\n{tail}")

    def _read_shell_prompt(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        output = bytearray()
        while time.monotonic() < deadline:
            data = self.serial.read(self.serial.in_waiting or 1)
            if not data:
                continue
            output.extend(data)
            self._record(data)
            prompt_input = ANSI_ESCAPE.sub(b"", bytes(output))
            if SHELL_PROMPT.search(prompt_input):
                self.last_read = bytes(output)
                return output.decode("utf-8", errors="replace")
        self.last_read = bytes(output)
        tail = output[-2000:].decode("utf-8", errors="replace")
        raise BootError(f"timeout waiting for MangoCore shell prompt; tail:\n{tail}")

    def acquire_prompt(self, timeout: float) -> None:
        self.serial.reset_input_buffer()
        self.serial.write(b"\r")
        self.serial.flush()
        try:
            self._read_prompt(1.5)
            print("[uboot] prompt already available")
            return
        except BootError:
            pass

        print(
            "[uboot] press the board RESET button now; waiting for a U-Boot "
            "autoboot banner before sending 'c'...",
            flush=True,
        )
        deadline = time.monotonic() + timeout
        output = bytearray(self.last_read)
        autoboot = autoboot_seen(output)
        next_interrupt = 0.0
        while time.monotonic() < deadline:
            now = time.monotonic()
            if autoboot and now >= next_interrupt:
                self.serial.write(b"c")
                self.serial.flush()
                next_interrupt = now + 0.2
            data = self.serial.read(self.serial.in_waiting or 1)
            if not data:
                continue
            output.extend(data)
            self._record(data)
            autoboot = autoboot or autoboot_seen(output)
            # The interrupt writer can race with the prompt and append one or
            # more `c` bytes immediately after "=> ".  Requiring the prompt
            # to be the exact buffer suffix then misses a prompt we already
            # reached and keeps flooding the command line until timeout.
            if prompt_seen(output):
                # Clear any extra 'c' that raced with the prompt before the
                # first real command is sent.
                self.serial.write(b"\x03\r")
                self.serial.flush()
                self._read_prompt(3)
                print("[uboot] autoboot interrupted")
                return
            if len(output) > 64 * 1024:
                del output[:-4096]
        if not autoboot:
            raise BootError(
                "U-Boot banner not observed; no autoboot interrupt bytes were sent; "
                "verify RESET, baud rate and serial selection"
            )
        raise BootError("U-Boot prompt not reached after the autoboot interrupt window")

    def command(self, command: str, timeout: float = 15) -> str:
        self.log.write(f"\n>>> {command}\n".encode("ascii"))
        self.serial.write(command.encode("ascii") + b"\r")
        self.serial.flush()
        output = self._read_prompt(timeout)
        if "Unknown command" in output:
            raise BootError(f"U-Boot does not support command: {command}")
        return output

    def shell_command(self, command: str, timeout: float = 30) -> str:
        marker = f"__MANGO_BOARD_RC_{os.getpid()}__"
        wrapped = f'{command}; rc=$?; printf "\\n{marker}%s\\n" "$rc"'
        encoded = wrapped.encode("ascii")
        if len(encoded) > 2048:
            raise BootError("board shell command exceeds the 2048-byte UART safety limit")
        self.log.write(f"\n>>> board {command}\n".encode("ascii"))
        self._write_console_input(encoded + b"\r")
        output = self._read_shell_prompt(timeout)
        match = re.search(re.escape(marker) + r"(-?\d+)", output)
        if match is None:
            raise BootError(
                f"board shell completion marker missing for {command!r}; "
                f"output:\n{output[-2000:]}"
            )
        return_code = int(match.group(1))
        if return_code != 0:
            raise BootError(
                f"board shell command failed with rc={return_code}: {command}; "
                f"output:\n{output[-2000:]}"
            )
        return output

    def boot_and_stream(self, loadaddr: str) -> None:
        command = f"bootm {loadaddr}"
        self.log.write(f"\n>>> {command}\n".encode("ascii"))
        self.serial.write(command.encode("ascii") + b"\r")
        self.serial.flush()
        print(
            "[console] booting; Ctrl-C -> board, Ctrl-] q -> close monitor, "
            "Ctrl-] ? -> help",
            flush=True,
        )

        self._stream_console()

    def boot_and_wait_for_shell(self, loadaddr: str, timeout: float) -> None:
        command = f"bootm {loadaddr}"
        self.log.write(f"\n>>> {command}\n".encode("ascii"))
        self.serial.write(command.encode("ascii") + b"\r")
        self.serial.flush()
        print("[console] booting; waiting for the MangoCore shell prompt", flush=True)
        # PID1 exposes the rescue console behind an "Enter to activate"
        # prompt.  Non-interactive FTP mode must activate it explicitly before
        # waiting for the shell prompt, otherwise the upload path stalls even
        # though the kernel has booted successfully.
        time.sleep(0.5)
        self.serial.write(b"\r")
        self.serial.flush()
        self._read_shell_prompt(timeout)
        print("[console] MangoCore shell ready", flush=True)

    def acquire_shell_prompt(self, timeout: float) -> None:
        """Attach to an already-running shell without resetting or booting."""
        self.serial.reset_input_buffer()
        self._write_console_input(b"\r")
        self._read_shell_prompt(timeout)
        print("[console] existing MangoCore shell ready", flush=True)

    def stream_console(self) -> None:
        self._stream_console()

    def monitor_existing_boot(self) -> None:
        print(
            "[console] serial-only SSD boot monitor; press board RESET if needed; "
            "no U-Boot command will be sent automatically; keyboard input is "
            "forwarded; Ctrl-] q -> close monitor, "
            "Ctrl-] ? -> help",
            flush=True,
        )
        self._stream_console()

    def _stream_console(self) -> None:

        try:
            stdin_fd: Optional[int] = sys.stdin.fileno()
        except (OSError, ValueError):
            stdin_fd = None
        serial_fd = self.serial.fileno()
        saved_terminal = None
        if stdin_fd is not None and sys.stdin.isatty():
            saved_terminal = termios.tcgetattr(stdin_fd)
            tty.setraw(stdin_fd)

        escape_pending = False
        stdin_open = True
        try:
            while True:
                read_fds = [serial_fd]
                if stdin_open and stdin_fd is not None:
                    read_fds.append(stdin_fd)
                readable, _, _ = select.select(read_fds, [], [], 0.2)
                if serial_fd in readable:
                    data = self.serial.read(self.serial.in_waiting or 1)
                    if data:
                        self._record(data)
                        sys.stdout.buffer.write(data)
                        sys.stdout.buffer.flush()

                if stdin_fd is not None and stdin_fd in readable:
                    data = os.read(stdin_fd, 1024)
                    if not data:
                        stdin_open = False
                        print(
                            "\n[console] stdin closed; continuing serial-only monitor "
                            "(Ctrl-] q to close)",
                            flush=True,
                        )
                        continue
                    should_close, escape_pending = self._handle_console_input(
                        data, escape_pending
                    )
                    if should_close:
                        return
        finally:
            if saved_terminal is not None and stdin_fd is not None:
                termios.tcsetattr(stdin_fd, termios.TCSADRAIN, saved_terminal)
            print("\n[console] monitor closed; board continues running")


def require(pattern: str, output: str, message: str) -> re.Match[str]:
    match = re.search(pattern, output, re.IGNORECASE | re.MULTILINE)
    if match is None:
        raise BootError(f"{message}; U-Boot output:\n{output[-2000:]}")
    return match


def detect_ftp_board_ip(args: argparse.Namespace, console: UBootConsole) -> str:
    if args.ftp_board_ip is not None:
        return args.ftp_board_ip
    output = console.shell_command("/bin/busybox ifconfig eth0", timeout=30)
    candidates = re.findall(
        r"\binet(?: addr:)?\s*([0-9]+(?:\.[0-9]+){3})", output, re.IGNORECASE
    )
    for candidate in candidates:
        try:
            return normalize_ipv4(candidate, "detected ftp_board_ip")
        except BootError:
            continue
    raise BootError(
        "could not detect a usable eth0 IPv4 address after boot; "
        "pass --ftp-board-ip explicitly"
    )


def http_upload_to_ssd(args: argparse.Namespace, console: UBootConsole) -> None:
    """Push a host file to P4 by serving it and pulling with board-side wget."""
    source = args.ftp_upload
    destination = args.ftp_destination
    if source is None or destination is None:
        raise BootError("HTTP upload configuration is incomplete")

    board_ip = detect_ftp_board_ip(args, console)
    source_size = source.stat().st_size
    source_sha256 = sha256(source)
    parent = posixpath.dirname(destination)
    temporary = f"{destination}.mango-upload-{os.getpid()}"
    host_url = f"http://{args.host_ip}:{args.ftp_port}/{quote(source.name)}"

    prefix = ipaddress.IPv4Network(f"0.0.0.0/{args.netmask}").prefixlen
    # Linux DHCP currently lands on 192.168.2.0/24 while the board-only host
    # link is 192.168.9.0/24.  Add the U-Boot-side address before wget so the
    # board can reach the temporary HTTP server without manual UART input.
    console.shell_command(
        f"/bin/busybox ip addr add {args.board_ip}/{prefix} dev eth0 "
        "2>/dev/null || true",
        timeout=90,
    )
    # /persist is already the mounted P4 root.  On the board, repeating
    # `mkdir -p` on that mountpoint can enter the MountFS/ext4 create path and
    # wait indefinitely even though the directory exists.  Only create a
    # nested parent when the destination actually needs one.
    if parent != "/persist":
        console.shell_command(
            f"/bin/busybox mkdir -p {shlex.quote(parent)}", timeout=90
        )
    server = subprocess.Popen(
        http_server_command(source.parent, args.host_ip, args.ftp_port),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    time.sleep(0.3)
    if server.poll() is not None:
        raise BootError("host HTTP server failed to start")
    print(
        f"[http] board {board_ip} downloading {host_url} -> {destination} "
        f"({source_size} bytes, sha256={source_sha256})",
        flush=True,
    )
    try:
        download = (
            f"/bin/busybox wget -O {shlex.quote(temporary)} "
            f"{shlex.quote(host_url)}"
        )
        console.shell_command(download, timeout=max(120, args.ftp_timeout))
        verify_and_commit = (
            f"test -f {shlex.quote(temporary)} && "
            f"actual_size=$(/bin/busybox wc -c < {shlex.quote(temporary)}) && "
            f"test \"$actual_size\" -eq {source_size} && "
            f"actual_sha=$(/bin/busybox sha256sum {shlex.quote(temporary)} "
            f"| /bin/busybox awk '{{print $1}}') && "
            f"test \"$actual_sha\" = {shlex.quote(source_sha256)} && "
            f"/bin/busybox mv -f {shlex.quote(temporary)} {shlex.quote(destination)} && "
            "/bin/busybox sync"
        )
        console.shell_command(verify_and_commit, timeout=max(60, args.ftp_timeout))
    finally:
        server.terminate()
        try:
            server.wait(timeout=2)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=2)
    print(
        f"[http] PASS: {destination} committed after size/SHA-256 verification "
        f"({source_size} bytes, sha256={source_sha256})",
        flush=True,
    )


def boot(args, serial_module, tftp_image: Path, serial_path: str) -> None:
    release_matching_screen(serial_path, args.takeover_screen)
    console = UBootConsole(serial_module, serial_path, args.log, args.baud)
    try:
        console.acquire_prompt(args.reset_timeout)
        commands = (
            f"setenv ipaddr {args.board_ip}",
            f"setenv serverip {args.host_ip}",
            f"setenv netmask {args.netmask}",
            f"setenv loadaddr {args.loadaddr}",
        )
        for command in commands:
            console.command(command)

        print(f"[uboot] ping {args.host_ip}", flush=True)
        ping = console.command(f"ping {args.host_ip}", timeout=20)
        require(rf"host\s+{re.escape(args.host_ip)}\s+is\s+alive", ping, "ping failed")

        print(f"[uboot] tftpboot {tftp_image.name}", flush=True)
        transfer = console.command(
            f"tftpboot {args.loadaddr} {tftp_image.name}", timeout=args.tftp_timeout
        )
        transferred = int(
            require(r"Bytes transferred\s*=\s*(\d+)", transfer, "TFTP failed").group(1)
        )
        expected_size = tftp_image.stat().st_size
        if transferred != expected_size:
            raise BootError(f"short TFTP transfer: {transferred} != {expected_size}")

        expected_crc = crc32(tftp_image)
        checksum = console.command(f"crc32 {args.loadaddr} 0x{expected_size:x}", timeout=60)
        actual_crc = int(
            require(r"==>\s*([0-9a-f]{8})", checksum, "U-Boot CRC missing").group(1),
            16,
        )
        if actual_crc != expected_crc:
            raise BootError(f"TFTP CRC mismatch: {actual_crc:08x} != {expected_crc:08x}")
        print(f"[uboot] transfer verified: {transferred} bytes, crc32={actual_crc:08x}")

        image_info = console.command(f"iminfo {args.loadaddr}", timeout=30)
        require(r"LoongArch", image_info, "uImage architecture check failed")
        require(r"Checksum\s+\.\.\.\s+OK", image_info, "uImage checksum failed")
        print("[uboot] iminfo verified: LoongArch, checksum OK")

        if args.ftp_upload:
            console.boot_and_wait_for_shell(args.loadaddr, args.reset_timeout)
            http_upload_to_ssd(args, console)
            console.stream_console()
            return
        if args.no_boot:
            print("[uboot] --no-boot selected; leaving board at the U-Boot prompt")
            return
        console.boot_and_stream(args.loadaddr)
    except KeyboardInterrupt:
        print("\n[console] monitor closed; board continues running")
    finally:
        console.close()


def upload_existing_shell(args, serial_module, serial_path: str) -> None:
    """Upload to a running board shell without rebuilding, TFTP, or rebooting."""
    release_matching_screen(serial_path, args.takeover_screen)
    console = UBootConsole(serial_module, serial_path, args.log, args.baud)
    try:
        console.acquire_shell_prompt(args.reset_timeout)
        http_upload_to_ssd(args, console)
        console.stream_console()
    except KeyboardInterrupt:
        print("\n[console] monitor closed; board continues running")
    finally:
        console.close()


def monitor_ssd_boot(args, serial_module, serial_path: str) -> None:
    release_matching_screen(serial_path, args.takeover_screen)
    console = UBootConsole(serial_module, serial_path, args.log, args.baud)
    try:
        console.monitor_existing_boot()
    except KeyboardInterrupt:
        print("\n[console] monitor closed; board continues running")
    finally:
        console.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "One-command macOS network setup, TFTP image transfer, optional "
            "FTP-to-SSD upload, serial detection, and Loongson 2K1000LA U-Boot boot"
        )
    )
    parser.add_argument("--interface", default="en8")
    parser.add_argument("--serial")
    parser.add_argument(
        "--image",
        type=Path,
        help="uImage path or filename to publish through TFTP and boot",
    )
    parser.add_argument(
        "--build-profile",
        choices=tuple(BUILD_PROFILES),
        help="build a 2K1000LA image before transfer (also supplies the image path)",
    )
    parser.add_argument(
        "--build-mode",
        choices=("release", "debug"),
        default="release",
        help="kernel build configuration when --build-profile is used",
    )
    parser.add_argument("--tftp-root", type=Path, default=DEFAULT_TFTP_ROOT)
    parser.add_argument("--host-ip", default="192.168.9.10")
    parser.add_argument("--board-ip", default="192.168.9.20")
    parser.add_argument(
        "--ftp-upload",
        type=Path,
        metavar="FILE",
        help="after boot, upload FILE to the board's writable SSD /persist tree",
    )
    parser.add_argument(
        "--upload-only",
        "--ftp-upload-only",
        action="store_true",
        help=(
            "upload FILE to an already-running P3 shell; skip build, TFTP, and reset "
            "(requires --ftp-upload)"
        ),
    )
    parser.add_argument(
        "--ftp-destination",
        "--ftp-dest",
        metavar="PATH",
        help="SSD destination for --ftp-upload (default: /persist/<filename>)",
    )
    parser.add_argument(
        "--ftp-board-ip",
        help="board IPv4 used by FTP after boot (default: detect eth0 from the shell)",
    )
    parser.add_argument(
        "--ftp-port",
        type=int,
        default=DEFAULT_FTP_PORT,
        help=f"temporary board FTP port (default: {DEFAULT_FTP_PORT})",
    )
    parser.add_argument(
        "--ftp-timeout",
        type=float,
        default=DEFAULT_FTP_TIMEOUT,
        help=f"host FTP upload timeout in seconds (default: {DEFAULT_FTP_TIMEOUT})",
    )
    parser.add_argument("--netmask", default="255.255.255.0")
    parser.add_argument("--loadaddr", default="0x9000000098000000")
    parser.add_argument("--baud", type=int, default=115200)
    parser.add_argument("--log", type=Path, default=Path("/private/tmp/mango-2k1000-boot.log"))
    parser.add_argument("--reset-timeout", type=float, default=120)
    parser.add_argument("--tftp-timeout", type=float, default=180)
    parser.add_argument("--no-host-config", dest="configure_host", action="store_false")
    parser.add_argument("--no-screen-takeover", dest="takeover_screen", action="store_false")
    interaction = parser.add_mutually_exclusive_group()
    interaction.add_argument(
        "--interactive",
        action="store_true",
        help="force the interactive setup wizard",
    )
    interaction.add_argument(
        "--non-interactive",
        action="store_true",
        help="disable the wizard; provide --image or --build-profile",
    )
    parser.add_argument(
        "--prepare-only",
        "--check-only",
        dest="prepare_only",
        action="store_true",
        help=(
            "configure/check the host, publish the image, and detect serial; "
            "do not open serial or send U-Boot commands"
        ),
    )
    parser.add_argument("--no-boot", action="store_true")
    parser.add_argument(
        "--monitor-only",
        action="store_true",
        help=(
            "only open the serial monitor for SSD autoboot; do not build, "
            "configure networking/TFTP, or automatically send board commands; "
            "manual keyboard input is forwarded"
        ),
    )
    parser.add_argument(
        "--install-ssd-kernel",
        action="store_true",
        help=(
            "rebuild and replace the complete SSD P3, installing the selected "
            "uImage as /boot/kernel-A.ui; requires the P4 backup gate"
        ),
    )
    parser.add_argument(
        "--ssd-backup-id",
        help="completed /persist/backups/<id> required by --install-ssd-kernel",
    )
    parser.add_argument(
        "--confirm-ssd-p3-start",
        help=f"destructive confirmation; must be {SSD_P3_START_LBA:#x}",
    )
    parser.add_argument(
        "--ssd-p3-image",
        type=Path,
        help="generated 768 MiB P3 payload path (must remain inside the repository)",
    )
    parser.add_argument(
        "--ssd-disk-image",
        type=Path,
        default=SSD_DISK_IMAGE,
        help="reference full-disk image used to verify the board MBR",
    )
    parser.set_defaults(configure_host=True, takeover_screen=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if sys.platform != "darwin":
        raise BootError("this host setup workflow currently supports macOS only")

    if args.build_profile and args.image is not None and args.non_interactive:
        raise BootError("--build-profile and --image cannot be combined in non-interactive mode")

    use_interactive = args.interactive or (args.image is None and not args.non_interactive)
    if use_interactive:
        if not (sys.stdin.isatty() and sys.stdout.isatty()):
            raise BootError(
                "interactive mode requires a terminal; "
                "pass --non-interactive with --image"
            )
        args = interactive_config(args)
    elif (
        not args.monitor_only
        and not args.upload_only
        and args.image is None
        and args.build_profile is None
    ):
        raise BootError("--image or --build-profile is required with --non-interactive")

    if args.monitor_only:
        if any(
            (
                args.image is not None,
                args.build_profile is not None,
                args.prepare_only,
                args.no_boot,
                args.install_ssd_kernel,
                args.ftp_upload is not None,
                args.ftp_destination is not None,
                args.ftp_board_ip is not None,
            )
        ):
            raise BootError(
                "--monitor-only cannot be combined with image/build/prepare/transfer/install options"
            )
        if args.baud <= 0:
            raise BootError("baud must be positive")
        serial_path = detect_serial(args.serial)
        print(f"[host] serial device: {serial_path}")
        try:
            import serial
        except ImportError as error:
            raise BootError(
                "pyserial is required: python3 -m pip install --user pyserial"
            ) from error
        monitor_ssd_boot(args, serial, serial_path)
        return

    if args.upload_only and any(
        (
            args.image is not None,
            args.build_profile is not None,
            args.prepare_only,
            args.no_boot,
            args.install_ssd_kernel,
        )
    ):
        raise BootError(
            "--upload-only cannot be combined with image/build/prepare/transfer/install options"
        )

    validate_cli_inputs(args)
    if not args.upload_only:
        args.tftp_root = validate_tftp_root(args.tftp_root)
    if args.upload_only and args.ftp_upload is None:
        raise BootError("--upload-only requires --ftp-upload FILE")
    if args.ftp_upload is not None and (
        args.prepare_only or args.no_boot or args.install_ssd_kernel
    ):
        raise BootError(
            "--ftp-upload requires a booting shell; it cannot be combined with "
            "--prepare-only, --no-boot, or --install-ssd-kernel"
        )
    if args.install_ssd_kernel:
        if args.prepare_only or args.no_boot:
            raise BootError(
                "--install-ssd-kernel cannot be combined with --prepare-only or --no-boot"
            )
        validate_ssd_install_gate(args)
    if args.build_profile:
        args.image = build_image(args.build_profile, args.build_mode)

    if args.upload_only:
        ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
        serial_path = detect_serial(args.serial)
        print(f"[host] serial device: {serial_path}")
        try:
            import serial
        except ImportError as error:
            raise BootError(
                "pyserial is required: python3 -m pip install --user pyserial"
            ) from error
        upload_existing_shell(args, serial, serial_path)
        return

    if args.install_ssd_kernel:
        install_ssd_kernel(args)
        return

    ensure_interface(args.interface, args.host_ip, args.netmask, args.configure_host)
    ensure_tftp_service(args.configure_host, args.tftp_root)
    tftp_image = prepare_tftp_image(args.image, args.tftp_root, require_uimage=True)
    serial_path = detect_serial(args.serial)
    print(f"[host] serial device: {serial_path}")
    if args.prepare_only:
        print("[host] prepare complete; serial was not opened and U-Boot was not touched")
        return

    try:
        import serial
    except ImportError as error:
        raise BootError("pyserial is required: python3 -m pip install --user pyserial") from error
    boot(args, serial, tftp_image, serial_path)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nERROR: interrupted", file=sys.stderr)
        raise SystemExit(130)
    except (BootError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
