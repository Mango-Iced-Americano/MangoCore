#!/usr/bin/env python3
"""
auto_exclude_glibc.py — 用 glibc 跑 ltp_include 列表，把 panic/超时的 case 写入 ltp_exclude_glibc

前提：ltp_include 已由 auto_include_ltp.py (musl) 填充完毕。

用法：
    python3 scripts/auto_exclude_glibc.py

环境变量：
    ARCH                  — rv64（默认）| la64
    TIMEOUT_SEC           — 无输出超时秒数（默认 15）
    HARD_TIMEOUT_SEC      — 单测例硬超时秒数（默认 30），超时即强杀
    HARD_ROUND_TIMEOUT_SEC — 整轮硬超时秒数（默认 120）
    CONF_FILE             — os_test.conf 路径
    LOG_DIR               — 日志输出目录
    MAX_ROUNDS            — 最大轮次（默认 200）
"""

import os
import re
import sys
import time
import signal
import select
import subprocess
from pathlib import Path

# ============================================================
# 配置
# ============================================================
REPO_ROOT = Path(__file__).resolve().parent.parent
ARCH = os.environ.get("ARCH", "rv64")
TIMEOUT_SEC = int(os.environ.get("TIMEOUT_SEC", "15"))
HARD_TIMEOUT_SEC = int(os.environ.get("HARD_TIMEOUT_SEC", "30"))
HARD_ROUND_TIMEOUT_SEC = int(os.environ.get("HARD_ROUND_TIMEOUT_SEC", "120"))
CONF_FILE = Path(os.environ.get("CONF_FILE", str(REPO_ROOT / "os_test.conf")))
LOG_DIR = Path(os.environ.get("LOG_DIR", str(REPO_ROOT / "testresult/auto_ltp")))
MAX_ROUNDS = int(os.environ.get("MAX_ROUNDS", "200"))
MASK_OVERRIDE = os.environ.get("MASK_OVERRIDE", "0x800")
TEMP_CONF = Path(f"/tmp/auto_exclude_glibc_{ARCH}.conf")

QEMU_PATTERNS = {
    "rv64": "qemu-system-riscv64",
    "la64": "qemu-system-loongarch64",
}

RE_PANIC = re.compile(r"panicked at|HEAP ALLOCATION FAILED|Exception\(")

# ============================================================
# 工具函数
# ============================================================
def log(msg: str) -> None:
    print(f"[auto-exclude-glibc] {msg}", flush=True)


def die(msg: str) -> None:
    print(f"[auto-exclude-glibc] ERROR: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


# ============================================================
# 架构 / 镜像路径
# ============================================================
def normalize_arch(arch: str) -> str:
    if arch in ("rv", "rv64"):
        return "rv64"
    if arch in ("la", "la64"):
        return "la64"
    die(f"unsupported ARCH='{arch}', expected rv64 or la64")


def resolve_blk_mode(arch: str) -> str:
    blk = os.environ.get("BLK_MODE", "")
    if blk:
        return blk
    return "virt" if arch == "rv64" else "virt-pci"


def resolve_run_target(arch: str) -> str:
    return "rv64-run" if arch == "rv64" else "la64-run"


def resolve_image_paths(arch: str):
    if arch == "rv64":
        return REPO_ROOT / "sdcard-rv.img", REPO_ROOT / "fs-img-dir/sdcard-rv.img.xz"
    else:
        return REPO_ROOT / "sdcard-la.img", REPO_ROOT / "fs-img-dir/sdcard-la.img.xz"


def restore_image(img_file: Path, img_backup: Path) -> bool:
    if not img_backup.exists():
        log(f"ERROR: backup image not found: {img_backup}, cannot restore")
        return False
    log(f"restoring {img_file} from {img_backup} ...")
    with open(img_file, "wb") as out:
        subprocess.run(["xz", "-dkc", str(img_backup)], stdout=out, check=True)
    log("restore done")
    return True


# ============================================================
# os_test.conf 读写
# ============================================================
def read_conf(key: str) -> str:
    try:
        for line in CONF_FILE.read_text().splitlines():
            if line.startswith(f"{key}="):
                return line.split("=", 1)[1]
    except FileNotFoundError:
        pass
    return ""


def write_conf(key: str, val: str) -> None:
    lines = []
    found = False
    try:
        for line in CONF_FILE.read_text().splitlines():
            if line.startswith(f"{key}="):
                lines.append(f"{key}={val}")
                found = True
            else:
                lines.append(line)
    except FileNotFoundError:
        pass
    if not found:
        lines.append(f"{key}={val}")
    CONF_FILE.write_text("\n".join(lines) + "\n")


def unique_list(raw: str) -> list[str]:
    if not raw:
        return []
    seen = set()
    result = []
    for part in raw.split(","):
        part = part.strip()
        if part and part not in seen:
            seen.add(part)
            result.append(part)
    return result


def append_item(lst: list[str], item: str) -> list[str]:
    if not item:
        return lst
    if item not in lst:
        lst.append(item)
    return lst


def list_to_conf(lst: list[str]) -> str:
    return ",".join(lst)


def write_temp_conf(
    include_list: list[str],
    exclude_glibc: list[str],
    from_case: str,
) -> None:
    """生成临时配置：ltp_libc=glibc，ltp_include 来自主 conf，exclude 只填 glibc 的"""
    base_lines = CONF_FILE.read_text().splitlines()
    skip_keys = {
        "mask", "ltp_include", "ltp_exclude",
        "ltp_exclude_musl", "ltp_exclude_glibc", "ltp_from", "ltp_libc",
    }
    output_lines = [
        line for line in base_lines
        if line.split("=", 1)[0] not in skip_keys
    ]
    output_lines.append(f"mask={MASK_OVERRIDE}")
    output_lines.append(f"ltp_include={list_to_conf(include_list)}")
    output_lines.append("ltp_exclude=")
    output_lines.append("ltp_exclude_musl=")
    output_lines.append(f"ltp_exclude_glibc={list_to_conf(exclude_glibc)}")
    output_lines.append(f"ltp_from={from_case}")
    output_lines.append("ltp_libc=glibc")
    TEMP_CONF.write_text("\n".join(output_lines) + "\n")


# ============================================================
# conf-inject（带重试）
# ============================================================
def conf_inject(
    conf_path: Path, img_file: Path, img_backup: Path,
    arch: str, blk_mode: str, max_retries: int = 3,
) -> bool:
    for attempt in range(1, max_retries + 1):
        result = subprocess.run(
            [
                "make", "-C", str(REPO_ROOT / "os"),
                "conf-inject",
                f"CONF_ARCH={arch}",
                f"CONF_BLK_MODE={blk_mode}",
                f"CONF_FILE={str(conf_path)}",
            ],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            return True
        log(f"conf-inject failed (attempt {attempt}/{max_retries}): {result.stderr.strip()}")
        if attempt < max_retries:
            log("restoring image before retry...")
            if not restore_image(img_file, img_backup):
                return False
    log(f"conf-inject failed after {max_retries} attempts, giving up")
    return False


# ============================================================
# 进程管理
# ============================================================
def kill_qemu(arch: str) -> None:
    pattern = QEMU_PATTERNS.get(arch, "")
    if pattern:
        subprocess.run(["pkill", "-f", pattern], capture_output=True)


# ============================================================
# 信号处理
# ============================================================
_run_proc: subprocess.Popen | None = None
_should_stop = False


def signal_handler(signum, frame):
    global _should_stop
    _should_stop = True
    log("interrupted, cleaning up...")


# ============================================================
# 运行 QEMU（与 auto_include 相同的非阻塞超时逻辑）
# ============================================================
def run_qemu_round(
    log_file: Path,
    run_target: str,
    arch: str,
    timeout_sec: int,
    hard_timeout_sec: int,
    hard_round_timeout_sec: int,
) -> tuple[bool, bool, str | None]:
    """
    运行一轮 QEMU，将输出写入 log_file。
    返回 (panic, timed_out, current_case)。
    """
    global _run_proc, _should_stop

    log_file.write_text("")

    make_dir = REPO_ROOT / "os"
    _run_proc = subprocess.Popen(
        ["make", "-C", str(make_dir), run_target],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    fd = _run_proc.stdout.fileno()
    os.set_blocking(fd, False)

    current_case: str | None = None
    panic = False
    timed_out = False

    case_start_time: float = 0.0
    round_start: float = time.time()
    last_activity: float = round_start

    log_lines: list[str] = []
    run_re = re.compile(r"RUN LTP CASE (.+)")

    buf = b""
    try:
        while True:
            if _should_stop:
                break

            _now = time.time()
            remaining_case = max(hard_timeout_sec - (_now - case_start_time), 0) if case_start_time > 0 else hard_timeout_sec
            remaining_round = max(hard_round_timeout_sec - (_now - round_start), 0)
            remaining_idle = max(timeout_sec - (_now - last_activity), 0)
            poll_timeout = min(remaining_case, remaining_round, remaining_idle)
            if poll_timeout <= 0:
                break

            ready, _, _ = select.select([fd], [], [], poll_timeout)
            _now = time.time()

            if case_start_time > 0 and (_now - case_start_time) >= hard_timeout_sec:
                timed_out = True
                log(f"hard timeout ({hard_timeout_sec}s) for case={current_case}")
                break
            if (_now - round_start) >= hard_round_timeout_sec:
                timed_out = True
                log(f"hard round timeout ({hard_round_timeout_sec}s), case={current_case}")
                break
            if (_now - last_activity) >= timeout_sec:
                timed_out = True
                log(f"no output timeout ({timeout_sec}s), case={current_case}")
                break

            if not ready:
                continue

            try:
                chunk = os.read(fd, 65536)
            except BlockingIOError:
                continue

            if not chunk:
                break

            buf += chunk
            while b"\n" in buf:
                raw_line_bytes, buf = buf.split(b"\n", 1)
                try:
                    line = raw_line_bytes.decode("utf-8", errors="replace").rstrip("\r")
                except Exception:
                    line = raw_line_bytes.decode("latin-1", errors="replace").rstrip("\r")

                log_lines.append(line)

                with open(log_file, "a") as f:
                    f.write(line + "\n")

                last_activity = time.time()

                m = run_re.search(line)
                if m:
                    current_case = m.group(1).strip()
                    case_start_time = time.time()
                    continue

                if RE_PANIC.search(line):
                    panic = True
                    log(f"PANIC/KERNEL EXCEPTION detected, case={current_case}")
                    break

            if panic:
                break

    except Exception as e:
        log(f"error reading QEMU output: {e}")
        panic = True

    kill_qemu(arch)
    if _run_proc and _run_proc.poll() is None:
        _run_proc.terminate()
        try:
            _run_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            _run_proc.kill()
    _run_proc = None

    if not current_case:
        for line in log_lines:
            m = run_re.search(line)
            if m:
                current_case = m.group(1).strip()
        if current_case:
            log(f"recovered current_case from log: {current_case}")

    return panic, timed_out, current_case


# ============================================================
# 扫描日志中的 panic（事后确认）
# ============================================================
def scan_log_for_panic(log_path: Path) -> tuple[bool, str | None]:
    """扫描日志文件，返回 (是否 panic, 最后一个 RUN case 名)"""
    if not log_path.exists():
        return False, None

    panic = False
    last_run_case: str | None = None
    run_re = re.compile(r"RUN LTP CASE (.+)")

    for line in log_path.read_text(errors="replace").splitlines():
        m = run_re.search(line)
        if m:
            last_run_case = m.group(1).strip()
            continue
        if RE_PANIC.search(line):
            panic = True

    return panic, last_run_case


# ============================================================
# 主逻辑
# ============================================================
def main():
    global _should_stop

    arch = normalize_arch(ARCH)
    blk_mode = resolve_blk_mode(arch)
    run_target = resolve_run_target(arch)
    img_file, img_backup = resolve_image_paths(arch)

    if not CONF_FILE.exists():
        die(f"CONF_FILE not found: {CONF_FILE}")

    LOG_DIR.mkdir(parents=True, exist_ok=True)

    # 加载状态
    include_list = unique_list(read_conf("ltp_include"))
    if not include_list:
        die("ltp_include is empty — run auto_include_ltp.py (musl) first")

    exclude_glibc_accum = unique_list(read_conf("ltp_exclude_glibc"))
    ltp_from = read_conf("ltp_from")

    log(f"start arch={arch} blk_mode={blk_mode} timeout={TIMEOUT_SEC}s")
    log(f"ltp_include has {len(include_list)} cases")
    log(f"ltp_exclude_glibc already has {len(exclude_glibc_accum)} cases")
    if ltp_from:
        log(f"ltp_from={ltp_from} (resuming)")

    if not restore_image(img_file, img_backup):
        die("initial image restore failed")

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    for round_num in range(1, MAX_ROUNDS + 1):
        if _should_stop:
            break

        write_temp_conf(include_list, exclude_glibc_accum, ltp_from)

        if ltp_from:
            log(f"round={round_num} ltp_from={ltp_from} glibc_exclude={len(exclude_glibc_accum)}")
        else:
            log(f"round={round_num} glibc_exclude={len(exclude_glibc_accum)}")

        if not conf_inject(TEMP_CONF, img_file, img_backup, arch, blk_mode):
            die("conf-inject failed repeatedly, aborting")

        log_file = LOG_DIR / f"exclude_glibc_round_{round_num}.log"

        # ---- 运行 QEMU ----
        panic, timed_out, current_case = run_qemu_round(
            log_file, run_target, arch,
            TIMEOUT_SEC, HARD_TIMEOUT_SEC, HARD_ROUND_TIMEOUT_SEC,
        )

        # ---- 事后扫描确认 panic ----
        scan_panic, scan_last = scan_log_for_panic(log_file)
        panic = panic or scan_panic
        if not current_case and scan_last:
            current_case = scan_last

        # ---- 处理 panic/超时 ----
        if panic or timed_out:
            if panic:
                log(f"panic detected, case={current_case}")
            else:
                log(f"timeout detected, case={current_case}")

            if current_case:
                exclude_glibc_accum = append_item(exclude_glibc_accum, current_case)
                ltp_from = current_case
                write_conf("ltp_exclude_glibc", list_to_conf(exclude_glibc_accum))
                write_conf("ltp_from", ltp_from)
                log(f"excluded (glibc): {current_case} (total={len(exclude_glibc_accum)})")

                if not conf_inject(CONF_FILE, img_file, img_backup, arch, blk_mode):
                    log("conf-inject for exclusion failed, restoring image")
                    restore_image(img_file, img_backup)
                continue
            else:
                log("no RUN LTP CASE line found, restoring image and retrying")
                restore_image(img_file, img_backup)
                continue

        # ---- 整轮跑完 ----
        log(f"round {round_num} finished without panic/timeout")
        break

    # 写回最终结果
    write_conf("ltp_exclude_glibc", list_to_conf(exclude_glibc_accum))
    write_conf("ltp_from", "")

    conf_inject(CONF_FILE, img_file, img_backup, arch, blk_mode)

    log("===== Final Results =====")
    log(f"ltp_exclude_glibc ({len(exclude_glibc_accum)} items)")
    log(f"  = {list_to_conf(exclude_glibc_accum)}")
    log(f"ltp_include total: {len(include_list)}")
    log(f"glibc-viable cases: {len(include_list) - len(exclude_glibc_accum)}")
    log("")
    log("os_test.conf updated")

    TEMP_CONF.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
