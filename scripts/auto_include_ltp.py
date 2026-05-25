#!/usr/bin/env python3
"""
auto_include_ltp.py — 多轮自动收集有 TPASS 的 LTP 测例（两阶段架构）

Phase 1 (运行): 控制 QEMU 启动/超时/Kill，写日志到文件。
Phase 2 (扫描): 解析已完成 round 的日志，提取有 TPASS 的 case。

用法：
    python3 scripts/auto_include_ltp.py

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
import threading
import subprocess
from pathlib import Path

# ============================================================
# 配置
# ============================================================
REPO_ROOT = Path(__file__).resolve().parent.parent
ARCH = os.environ.get("ARCH", "rv64")
TIMEOUT_SEC = int(os.environ.get("TIMEOUT_SEC", "30"))
HARD_TIMEOUT_SEC = int(os.environ.get("HARD_TIMEOUT_SEC", "60"))
HARD_ROUND_TIMEOUT_SEC = int(os.environ.get("HARD_ROUND_TIMEOUT_SEC", "120"))
CONF_FILE = Path(os.environ.get("CONF_FILE", str(REPO_ROOT / "os_test.conf")))
LOG_DIR = Path(os.environ.get("LOG_DIR", str(REPO_ROOT / "testresult/auto_ltp")))
MAX_ROUNDS = int(os.environ.get("MAX_ROUNDS", "200"))
MASK_OVERRIDE = os.environ.get("MASK_OVERRIDE", "0x800")
TEMP_CONF = Path(f"/tmp/auto_include_{ARCH}.conf")
SCAN_ONLY = os.environ.get("SCAN_ONLY", "0") == "1"

# QEMU kill patterns
QEMU_PATTERNS = {
    "rv64": "qemu-system-riscv64",
    "la64": "qemu-system-loongarch64",
}

# ============================================================
# 工具函数
# ============================================================
def log(msg: str) -> None:
    print(f"[auto-include] {msg}", flush=True)


def die(msg: str) -> None:
    print(f"[auto-include] ERROR: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


# ============================================================
# ANSI 转义码剥离
# ============================================================
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")


def strip_ansi(s: str) -> str:
    return ANSI_RE.sub("", s)


def normalize_case_name(name: str) -> str:
    if name.endswith(".sh"):
        return name[:-3]
    return name


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
    """同步解压镜像（仅在初始化时使用）。"""
    if not img_backup.exists():
        log(f"ERROR: backup image not found: {img_backup}, cannot restore")
        return False
    log(f"restoring {img_file} from {img_backup} ...")
    with open(img_file, "wb") as out:
        subprocess.run(["xz", "-dkc", str(img_backup)], stdout=out, check=True)
    log("restore done")
    return True


def bkg_decompress(img_backup: Path, output_path: Path):
    """后台解压镜像到临时文件。失败时删除不完整文件。"""
    try:
        with open(output_path, "wb") as f:
            subprocess.run(
                ["xz", "-dkc", str(img_backup)],
                stdout=f, check=True,
            )
    except Exception as e:
        log(f"background decompress failed: {e}")
        output_path.unlink(missing_ok=True)


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
    exclude: list[str],
    exclude_musl: list[str],
    exclude_glibc: list[str],
    from_case: str,
) -> None:
    """生成临时配置文件（ltp_include 置空 + ltp_libc 固定为 musl）"""
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
    output_lines.append("ltp_include=")
    output_lines.append(f"ltp_exclude={list_to_conf(exclude)}")
    output_lines.append(f"ltp_exclude_musl={list_to_conf(exclude_musl)}")
    output_lines.append(f"ltp_exclude_glibc={list_to_conf(exclude_glibc)}")
    output_lines.append(f"ltp_from={from_case}")
    output_lines.append("ltp_libc=musl")
    TEMP_CONF.write_text("\n".join(output_lines) + "\n")


# ============================================================
# conf-inject（带重试）
# ============================================================
def conf_inject(
    conf_path: Path, arch: str, blk_mode: str, max_retries: int = 2,
) -> bool:
    """将配置文件注入镜像。每轮镜像都是新鲜的，只需简单重试。"""
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
# Phase 2: 日志扫描 — 从 round log 中提取有 TPASS 的 case
# ============================================================

# LTP TPASS 格式（剥离 ANSI 后）：
#   格式 A:  case_name    N  TPASS  :  message
#      例:   abs01       1  TPASS  :  Test passed
#   格式 B:  file.c:line: TPASS: message
#      例:   abort01.c:65: TPASS: abort() raised SIGIOT

# 格式 A: "case_name   N  TPASS  :" 或 "case_name   N  TPASS:"
RE_TPASS_A = re.compile(r"^(\S+)\s+\d+\s+TPASS\s*:")

# 格式 B: "file.c:line: TPASS:"
RE_TPASS_B = re.compile(r"^(\S+)\.c:\d+:\s*TPASS:")

# 捕获 panic / kernel exception
RE_PANIC = re.compile(r"panicked at|HEAP ALLOCATION FAILED|Exception\(")


def scan_log_for_tpass(log_path: Path) -> tuple[set[str], bool, str | None]:
    """
    扫描日志文件，返回 (有 TPASS 的 case 集合, 是否 panic, 最后一个 RUN case 名)

    通过 RUN LTP CASE 记录当前 case，并用它来归一化匹配 TPASS 行。
    这样既能处理 .sh 脚本，也能处理不带后缀的 LTP case。
    """
    tpass_cases: set[str] = set()
    panic = False
    last_run_case: str | None = None

    if not log_path.exists():
        return tpass_cases, panic, last_run_case

    run_re = re.compile(r"RUN LTP CASE (.+)")
    current_case: str | None = None

    for raw_line in log_path.read_text(errors="replace").splitlines():
        raw_line = raw_line.strip()
        if not raw_line:
            continue

        # 跟踪当前/最后一个 RUN case
        m = run_re.search(raw_line)
        if m:
            last_run_case = m.group(1).strip()
            current_case = last_run_case
            continue

        # 检测 panic
        if RE_PANIC.search(raw_line):
            panic = True
            continue

        line = strip_ansi(raw_line)

        # 格式 A: case_name   N  TPASS  :
        m = RE_TPASS_A.search(line)
        if m and current_case and normalize_case_name(m.group(1)) == normalize_case_name(current_case):
            tpass_cases.add(current_case)
            continue

        # 格式 B: file.c:line: TPASS:
        m = RE_TPASS_B.search(line)
        if m and current_case and normalize_case_name(m.group(1)) == normalize_case_name(current_case):
            tpass_cases.add(current_case)
            continue

    return tpass_cases, panic, last_run_case


def collect_include_from_logs() -> list[str]:
    include_accum = unique_list(read_conf("ltp_include"))

    for round_file in sorted(LOG_DIR.glob("include_round_*.log")):
        tpass_cases, _has_panic, _last_run = scan_log_for_tpass(round_file)
        for case_name in sorted(tpass_cases):
            if case_name not in include_accum:
                include_accum = append_item(include_accum, case_name)
                log(f"rescued include candidate: {case_name} from {round_file.name}")

    return include_accum


# ============================================================
# Phase 1: 运行 QEMU
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

            # ---- 超时检查 ----
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
                break  # EOF

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

                # ---- RUN LTP CASE ----
                m = run_re.search(line)
                if m:
                    current_case = m.group(1).strip()
                    case_start_time = time.time()
                    continue

                # ---- PANIC / KERNEL EXCEPTION ----
                if RE_PANIC.search(line):
                    panic = True
                    log(f"PANIC/KERNEL EXCEPTION detected, case={current_case}")
                    break

            if panic:
                break

    except Exception as e:
        log(f"error reading QEMU output: {e}")
        panic = True

    # ---- 清理 QEMU ----
    kill_qemu(arch)
    if _run_proc and _run_proc.poll() is None:
        _run_proc.terminate()
        try:
            _run_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            _run_proc.kill()
    _run_proc = None

    # ---- 如果 current_case 为空，从日志恢复 ----
    if not current_case:
        for line in log_lines:
            m = run_re.search(line)
            if m:
                current_case = m.group(1).strip()
        if current_case:
            log(f"recovered current_case from log: {current_case}")

    return panic, timed_out, current_case


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

    if SCAN_ONLY:
        log(f"scan-only mode enabled, scanning {LOG_DIR} for existing round logs")
        include_accum = collect_include_from_logs()
        write_conf("ltp_include", list_to_conf(include_accum))
        write_conf("ltp_from", "")
        log("===== Final Results =====")
        log(f"ltp_include ({len(include_accum)} items) = {list_to_conf(include_accum)}")
        log("")
        log(f"done — {len(include_accum)} cases in include list")
        log("os_test.conf updated")
        return

    # 加载已有状态（支持断点续跑）
    include_accum = unique_list(read_conf("ltp_include"))
    exclude_accum = unique_list(read_conf("ltp_exclude"))
    exclude_musl_accum = unique_list(read_conf("ltp_exclude_musl"))
    exclude_glibc_accum = unique_list(read_conf("ltp_exclude_glibc"))
    ltp_from = read_conf("ltp_from")

    log(f"start arch={arch} blk_mode={blk_mode} timeout={TIMEOUT_SEC}s")
    log(f"include_accum={len(include_accum)} so far")
    log(f"exclude_accum={len(exclude_accum)} so far")
    if ltp_from:
        log(f"ltp_from={ltp_from} (will skip passed cases)")

    # 恢复原始镜像作为起点（同步解压）
    if not restore_image(img_file, img_backup):
        die("initial image restore failed")

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    # ============ 补救：从已有 round log 中恢复 include ============
    for round_file in sorted(LOG_DIR.glob("include_round_*.log")):
        tpass_cases, has_panic, _last_run = scan_log_for_tpass(round_file)
        if tpass_cases:
            added = 0
            for c in tpass_cases:
                if c not in include_accum:
                    include_accum = append_item(include_accum, c)
                    added += 1
            if added:
                log(f"rescued {added} cases from existing {round_file.name}")

    # ============ 主循环 ============
    bg_decompress_thread: threading.Thread | None = None
    next_img_path = Path(str(img_file) + ".next")

    for round_num in range(1, MAX_ROUNDS + 1):
        if _should_stop:
            break

        # 等上一轮后台解压完成 → 原子替换镜像（首轮跳过）
        if bg_decompress_thread is not None:
            bg_decompress_thread.join()
            if next_img_path.exists():
                os.replace(next_img_path, img_file)
                log(f"swapped fresh image for round {round_num}")
            else:
                log(f"WARNING: background decompress did not produce {next_img_path}, re-decompressing")
                if not restore_image(img_file, img_backup):
                    die("re-decompress failed")
            bg_decompress_thread = None

        write_temp_conf(exclude_accum, exclude_musl_accum, exclude_glibc_accum, ltp_from)

        if ltp_from:
            log(f"round={round_num} ltp_from={ltp_from} include={len(include_accum)} exclude={len(exclude_accum)}")
        else:
            log(f"round={round_num} include={len(include_accum)} exclude={len(exclude_accum)}")

        if not conf_inject(TEMP_CONF, arch, blk_mode):
            die("conf-inject failed repeatedly, aborting")

        # 启动后台解压，为下一轮准备镜像（QEMU 运行时并行解压）
        bg_decompress_thread = threading.Thread(
            target=bkg_decompress, args=(img_backup, next_img_path),
            daemon=True,
        )
        bg_decompress_thread.start()

        log_file = LOG_DIR / f"include_round_{round_num}.log"

        # ---- Phase 1: 运行 QEMU ----
        panic, timed_out, current_case = run_qemu_round(
            log_file, run_target, arch,
            TIMEOUT_SEC, HARD_TIMEOUT_SEC, HARD_ROUND_TIMEOUT_SEC,
        )

        # ---- Phase 2: 扫描日志提取 TPASS ----
        tpass_cases, scan_panic, scan_last_run = scan_log_for_tpass(log_file)
        panic = panic or scan_panic

        if not current_case and scan_last_run:
            current_case = scan_last_run

        # 合并本轮 TPASS case
        if tpass_cases:
            added = 0
            for c in sorted(tpass_cases):
                if c not in include_accum:
                    include_accum = append_item(include_accum, c)
                    log(f"  include candidate: {c}")
                    added += 1
            if added:
                log(f"round_include={added} new items")

        # ---- 处理 panic/超时 ----
        if panic or timed_out:
            if panic:
                log(f"panic detected, case={current_case}")
            else:
                log(f"timeout detected, case={current_case}")

            if current_case:
                exclude_musl_accum = append_item(exclude_musl_accum, current_case)
                ltp_from = current_case
                write_conf("ltp_exclude_musl", list_to_conf(exclude_musl_accum))
                write_conf("ltp_from", ltp_from)
                log(f"excluded (musl): {current_case}")
                continue
            else:
                log("no RUN LTP CASE line found in log, cannot determine current case")
                continue

        # ---- 整轮跑完 ----
        log(f"round {round_num} finished without panic/timeout")
        break

    # ============= 写回 include 列表 =============
    include_accum = unique_list(list_to_conf(include_accum))
    write_conf("ltp_include", list_to_conf(include_accum))
    write_conf("ltp_from", "")

    # 清理临时文件
    next_img_path.unlink(missing_ok=True)

    log("===== Final Results =====")
    log(f"ltp_include ({len(include_accum)} items) = {list_to_conf(include_accum)}")
    log("")
    log(f"done — {len(include_accum)} cases in include list")
    log(f"      {len(exclude_musl_accum)} cases in musl exclude list")
    log(f"      {len(exclude_accum)} cases in global exclude list")
    log("")
    log("os_test.conf updated")

    TEMP_CONF.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
