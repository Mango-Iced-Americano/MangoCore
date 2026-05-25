#!/usr/bin/env python3
"""
一键全量测试脚本

在 Docker 环境中一键完成：
  1. make all                          — 编译 rv64 + la64 双架构内核
  2. 解压 sdcard 镜像                   — xz -dkc → sdcard-{rv,la}.img
  3. 并行 rv64 + la64 QEMU 运行        — 10min 超时保护
  4. judge/run_parse.py 评分           — 对两架构输出分别评分
  5. 终端汇总输出 + 结果留档            — archive_{timestamp}/

用法（项目根目录，在 Docker 内）：
    python3 scripts/run_full_test.py
"""

import subprocess
import threading
import os
import sys
import time
import json
import shutil
from datetime import datetime

# ======================== Paths ========================
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(SCRIPT_DIR)
TESTRESULT_DIR = os.path.join(PROJECT_ROOT, "testresult")
JUDGE_DIR = os.path.join(PROJECT_ROOT, "judge")
FS_IMG_DIR = os.path.join(PROJECT_ROOT, "fs-img-dir")

QEMU_TIMEOUT = int(os.environ.get("QEMU_TIMEOUT", "1800"))  # 默认 30 分钟

# ANSI colors
GREEN = "\033[92m"
RED = "\033[91m"
YELLOW = "\033[93m"
CYAN = "\033[96m"
BOLD = "\033[1m"
RESET = "\033[0m"

qemu_results = {}       # threading-safe dict for QEMU results
judge_results = {}      # threading-safe dict for judge results


# ======================== Helpers ========================

def log(msg, color=""):
    """带时间戳的日志打印。"""
    ts = datetime.now().strftime("%H:%M:%S")
    print(f"[{ts}] {color}{msg}{RESET}")


def run_cmd(cmd, cwd=None, timeout=None):
    """同步执行 shell 命令，返回 (rc, stdout)。"""
    log(f"$ {cmd}", CYAN)
    try:
        r = subprocess.run(
            cmd, shell=True, capture_output=False, text=True, cwd=cwd, timeout=timeout
        )
        if r.returncode != 0:
            log(f"命令失败 (rc={r.returncode}): {cmd}", RED)
        return r.returncode
    except subprocess.TimeoutExpired:
        log(f"命令超时: {cmd}", RED)
        return -1
    except Exception as e:
        log(f"命令异常: {e}", RED)
        return -1


# ======================== QEMU 运行（照 run_qemu.py 的模式）=======================

def run_qemu_instance(cmd, output_path, timeout, result_key):
    """
    启动一个 QEMU 进程，实时捕获 stdout+stderr 写入 output_path。
    超时则 kill。结果存入 qemu_results[result_key]。

    用 block 读取 + wall-clock 计时实现超时：启动时记录时间，
    read 每次超时检查是否超过 timeout，超时则强杀进程。
    与 auto_include_ltp.py 的 run_qemu_round 模式一致。
    """
    import select as _select

    log(f"QEMU [{result_key}] 启动，输出 → {output_path}")
    os.makedirs(os.path.dirname(output_path), exist_ok=True)

    p = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        stdin=subprocess.PIPE,
        shell=True,
    )

    try:
        p.stdin.write(b"\n")
        p.stdin.flush()
        p.stdin.close()
    except Exception:
        pass

    fd = p.stdout.fileno()
    os.set_blocking(fd, False)

    started = time.time()
    timed_out = False

    try:
        with open(output_path, "wb", buffering=1) as f:
            f.write(f"# QEMU CMD: {cmd}\n".encode("utf-8", errors="replace"))
            f.write(f"# Started: {datetime.now().isoformat()}\n".encode("utf-8", errors="replace"))
            f.write(b"#" * 60 + b"\n")
            f.flush()

            buf = b""
            while True:
                remaining = timeout - (time.time() - started)
                if remaining <= 0:
                    timed_out = True
                    break

                ready, _, _ = _select.select([fd], [], [], min(remaining, 5.0))
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
                    raw_line, buf = buf.split(b"\n", 1)
                    try:
                        line = raw_line.decode("utf-8", errors="replace") + "\n"
                    except Exception:
                        line = raw_line.decode("latin-1", errors="replace") + "\n"
                    f.write(line.encode("utf-8"))
                    f.flush()

                if buf:
                    try:
                        line = buf.decode("utf-8", errors="replace")
                    except Exception:
                        line = buf.decode("latin-1", errors="replace")
                    f.write(line.encode("utf-8"))
                    f.flush()
                    buf = b""
    finally:
        if timed_out:
            log(f"QEMU [{result_key}] 超时 ({timeout}s)，正在 kill...", YELLOW)
            p.kill()
            try:
                for raw_line in p.stdout:
                    pass
            except Exception:
                pass
        p.wait()

    rc = p.returncode
    qemu_results[result_key] = {
        "rc": rc,
        "timed_out": timed_out,
        "killed": timed_out,
        "output": output_path,
    }

    fname = os.path.basename(output_path)
    if timed_out:
        log(f"{fname}: 被终止（超时 {timeout}s）", YELLOW)
    elif rc != 0:
        log(f"{fname}: QEMU 异常退出 (rc={rc})", RED)
    else:
        log(f"{fname}: 正常结束", GREEN)


def build_rv64_cmd():
    """构建 rv64 QEMU 命令，参数参照 os/make/rv64.mk 的 comp 目标。"""
    return (
        "qemu-system-riscv64 "
        "-machine virt "
        "-kernel kernel-rv "
        "-m 1024 "
        "-nographic "
        "-smp 1 "
        "-bios default "
        "-drive file=sdcard-rv.img,if=none,format=raw,id=x0 "
        "-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 "
        "-no-reboot "
        "-rtc base=utc "
        "-device virtio-net-device,netdev=net "
        "-netdev user,id=net"
    )


def build_la64_cmd():
    """构建 la64 QEMU 命令，参数参照 os/make/la64o.mk 的 comp 目标。"""
    return (
        "qemu-system-loongarch64 "
        "-kernel kernel-la "
        "-m 1G "
        "-nographic "
        "-smp 1 "
        "-drive file=sdcard-la.img,if=none,format=raw,id=x0 "
        "-device virtio-blk-pci,drive=x0 "
        "-no-reboot "
        "-device virtio-net-pci,netdev=net0 "
        "-netdev user,id=net0 "
        "-rtc base=utc"
    )


# ======================== 评分 ========================

def run_judge_parse(output_file, arch_name):
    """调用 judge/run_parse.py 评分，返回解析后的结果 dict。"""
    log(f"评分 [{arch_name}] ← {output_file}", CYAN)
    try:
        r = subprocess.run(
            [sys.executable, os.path.join(JUDGE_DIR, "run_parse.py"),
             output_file, JUDGE_DIR],
            capture_output=True, text=True, timeout=120,
        )
        return {
            "success": r.returncode == 0,
            "stdout": r.stdout,
            "stderr": r.stderr,
            "returncode": r.returncode,
        }
    except subprocess.TimeoutExpired:
        return {"success": False, "error": "评分进程超时"}
    except Exception as e:
        return {"success": False, "error": str(e)}


def extract_json_from_judge(judge_stdout):
    """从 judge 输出的 Full JSON 行之后提取 JSON 对象。"""
    lines = judge_stdout.split("\n")
    for i, line in enumerate(lines):
        if line.strip().startswith("Full JSON:"):
            try:
                return json.loads("\n".join(lines[i + 1:]))
            except json.JSONDecodeError:
                return None
    return None


def parse_judge_table(judge_stdout):
    """
    解析 judge 输出的 GROUP/PASS/ALL 汇总表格。
    返回 {group_name: (pass_count, all_count)} 字典。
    """
    lines = judge_stdout.split("\n")
    # 找 ==== 分隔线的行号
    eq_indices = [i for i, line in enumerate(lines) if line.startswith("=" * 10)]
    if len(eq_indices) < 3:
        return {}

    # 数据在第 2 个 ==== 之后、最后一个 ==== 之前
    # 结构: ==== / GROUP... / ==== / data... / ==== / TOTAL / (blank) / Full JSON:
    start = eq_indices[1] + 1    # 第 2 个 ==== 之后
    end = eq_indices[-1] - 1     # 最后一个 ==== 之前

    groups = {}
    for line in lines[start:end + 1]:
        line = line.strip()
        if not line or line.startswith("GROUP") or line.startswith("TOTAL"):
            continue
        # 格式: "  basic-glibc                        0    102"
        parts = line.rsplit(None, 2)  # 从右分割2次
        if len(parts) == 3:
            name = parts[0]
            try:
                p = int(parts[1])
                a = int(parts[2])
                groups[name] = (p, a)
            except ValueError:
                pass
    return groups


# ======================== 主流程 ========================

def main():
    os.chdir(PROJECT_ROOT)
    log("=" * 60, BOLD)
    log("  MangoCore (oskernel2026) — 全量测试脚本", BOLD)
    log(f"  项目根目录: {PROJECT_ROOT}", BOLD)
    log(f"  开始时间: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}", BOLD)
    log(f"  QEMU 超时: {QEMU_TIMEOUT}s", BOLD)
    log("=" * 60, BOLD)

    # ========== Phase 1+2: 编译 + 解压镜像（并行执行）==========
    # make all 与两个 xz 解压互不依赖，可以同时进行
    log("\n" + "=" * 50, BOLD)
    log("Phase 1-2/6: 编译内核 + 解压 sdcard 镜像（并行）", BOLD)
    log("=" * 50, BOLD)

    rv_xz = os.path.join(FS_IMG_DIR, "sdcard-rv.img.xz")
    la_xz = os.path.join(FS_IMG_DIR, "sdcard-la.img.xz")

    # 先确保 xz 文件存在（串行下载，不影响大局）
    if not os.path.exists(rv_xz):
        log("未找到 sdcard-rv.img.xz，尝试下载...", YELLOW)
        run_cmd("make testsuits-download", cwd=PROJECT_ROOT)

    if not os.path.exists(la_xz):
        log("未找到 sdcard-la.img.xz，尝试下载...", YELLOW)
        run_cmd("make testsuits-download", cwd=PROJECT_ROOT)

    # 再检查一次，仍然没有就终止
    if not os.path.exists(rv_xz) or not os.path.exists(la_xz):
        log("❌ sdcard 镜像 xz 文件不存在，无法继续", RED)
        sys.exit(1)

    build_ok = [True]
    extract_ok = [True, True]  # [rv, la]

    def do_build():
        rc = run_cmd("make all", cwd=PROJECT_ROOT)
        build_ok[0] = (rc == 0)

    def do_extract(arch_label, xz_path, out_name):
        nonlocal extract_ok
        rc = run_cmd(f"xz -dkc '{xz_path}' > {out_name}", cwd=PROJECT_ROOT)
        idx = 0 if "rv" in arch_label else 1
        extract_ok[idx] = (rc == 0)

    build_thread = threading.Thread(target=do_build, name="make-all")
    extract_rv = threading.Thread(target=do_extract, args=("rv", rv_xz, "sdcard-rv.img"), name="xz-rv")
    extract_la = threading.Thread(target=do_extract, args=("la", la_xz, "sdcard-la.img"), name="xz-la")

    build_thread.start()
    extract_rv.start()
    extract_la.start()

    build_thread.join()
    extract_rv.join()
    extract_la.join()

    if not build_ok[0]:
        log("❌ make all 失败，终止脚本", RED)
        sys.exit(1)
    log("✅ make all 完成", GREEN)

    if not extract_ok[0]:
        log("❌ 解压 sdcard-rv.img.xz 失败", RED)
        sys.exit(1)

    if not extract_ok[1]:
        log("❌ 解压 sdcard-la.img.xz 失败", RED)
        sys.exit(1)

    log("✅ 镜像就绪", GREEN)

    # ========== Phase 3: 并行 QEMU ==========
    log("\n" + "=" * 50, BOLD)
    log(f"Phase 3/6: 并行 QEMU 运行（每架构超时 {QEMU_TIMEOUT}s）", BOLD)
    log("=" * 50, BOLD)

    rv64_cmd = build_rv64_cmd()
    la64_cmd = build_la64_cmd()
    rv64_output = os.path.join(TESTRESULT_DIR, "output-rv.txt")
    la64_output = os.path.join(TESTRESULT_DIR, "output-la.txt")

    # 并行启动 rv64 和 la64 QEMU（两个独立 QEMU 进程，互不依赖）
    rv_thread = threading.Thread(
        target=run_qemu_instance,
        args=(rv64_cmd, rv64_output, QEMU_TIMEOUT, "rv64"),
        name="qemu-rv64",
    )
    la_thread = threading.Thread(
        target=run_qemu_instance,
        args=(la64_cmd, la64_output, QEMU_TIMEOUT, "la64"),
        name="qemu-la64",
    )
    rv_thread.start()
    la_thread.start()
    rv_thread.join()
    la_thread.join()

    log("✅ QEMU 运行完毕", GREEN)

    # ========== Phase 4: 评分 ==========
    log("\n" + "=" * 50, BOLD)
    log("Phase 4/6: 评分解析 (judge/run_parse.py)", BOLD)
    log("=" * 50, BOLD)

    for arch in ["rv64", "la64"]:
        if arch not in qemu_results:
            log(f"⚠ [{arch}] QEMU 未运行，跳过评分", YELLOW)
            continue
        output_file = qemu_results[arch]["output"]
        if not os.path.exists(output_file) or os.path.getsize(output_file) == 0:
            log(f"⚠ [{arch}] 输出文件为空或不存在，跳过评分", YELLOW)
            continue
        judge_results[arch] = run_judge_parse(output_file, arch)

    # ========== Phase 5: 终端汇总输出 ==========
    log("\n" + "=" * 70, BOLD)
    log("Phase 5/6: 测试结果汇总", BOLD)
    log("=" * 70, BOLD)

    overall_ok = True

    for arch in ["rv64", "la64"]:
        print()
        print(f"{BOLD}{'═' * 30}")
        print(f"  {arch.upper()} 架构")
        print(f"{'═' * 30}{RESET}")

        # QEMU 状态
        if arch in qemu_results:
            qr = qemu_results[arch]
            status_icon = "⏱" if qr.get("timed_out") else ("✅" if qr["rc"] == 0 else "❌")
            status_text = ("超时终止" if qr.get("timed_out")
                           else ("正常结束" if qr["rc"] == 0 else f"异常退出 (rc={qr['rc']})"))
            print(f"  QEMU: {status_icon} {status_text}")
        else:
            print(f"  QEMU: ⚠️ 未运行")
            overall_ok = False

        # Judge 结果
        if arch in judge_results:
            jr = judge_results[arch]
            if jr["success"]:
                # 直接输出 judge 的标准 stdout（包含 GROUP/PASS/ALL 表和 JSON）
                print(jr["stdout"])
            else:
                print(f"  评分失败: {jr.get('error', '未知错误')}")
                if jr.get("stderr"):
                    print(f"  stderr: {jr['stderr']}")
                overall_ok = False
        else:
            print(f"  评分: ⚠️ 未执行")

    # ---- 双架构总分汇总 ----
    print()
    print(f"{BOLD}{'═' * 60}")
    print(f"  💯 双架构总分汇总 (RV64 + LA64)")
    print(f"{'═' * 60}{RESET}")
    print(f"  {'GROUP':<28} {'PASS':>6} {'ALL':>6}")
    print(f"  {'─' * 42}")

    # 解析两架构各自的表格数据
    all_groups = {}  # group_name -> (pass_rv, all_rv, pass_la, all_la)
    for arch in ["rv64", "la64"]:
        if arch not in judge_results or not judge_results[arch]["success"]:
            continue
        table = parse_judge_table(judge_results[arch]["stdout"])
        for name, (p, a) in table.items():
            if name not in all_groups:
                all_groups[name] = [0, 0, 0, 0]
            if arch == "rv64":
                all_groups[name][0] += p
                all_groups[name][1] += a
            else:
                all_groups[name][2] += p
                all_groups[name][3] += a

    grand_pass = 0
    grand_all = 0
    for name in sorted(all_groups.keys()):
        rv_p, rv_a, la_p, la_a = all_groups[name]
        total_p = rv_p + la_p
        total_a = rv_a + la_a
        grand_pass += total_p
        grand_all += total_a
        print(f"  {name:<28}  {total_p:>6} {total_a:>6}")

    print(f"  {'─' * 42}")
    print(f"  {'TOTAL':<28}  {grand_pass:>6} {grand_all:>6}")
    print(f"  {'═' * 42}")
    print()

    # ========== Phase 6: 结果留档 ==========
    log("\n" + "=" * 50, BOLD)
    log("Phase 6/6: 结果留档", BOLD)
    log("=" * 50, BOLD)

    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    archive_dir = os.path.join(TESTRESULT_DIR, f"archive_{timestamp}")
    os.makedirs(archive_dir, exist_ok=True)

    # 拷贝 QEMU 输出文件
    for arch in ["rv64", "la64"]:
        src = os.path.join(TESTRESULT_DIR, f"output-{arch}.txt")
        if os.path.exists(src):
            shutil.copy2(src, os.path.join(archive_dir, f"output-{arch}.txt"))
            log(f"  📄 output-{arch}.txt → {archive_dir}/")

    # 写入 summary.txt
    summary_path = os.path.join(archive_dir, "summary.txt")
    with open(summary_path, "w", encoding="utf-8") as f:
        f.write("=" * 60 + "\n")
        f.write(f"MangoCore (oskernel2026) — 全量测试报告\n")
        f.write(f"测试时间: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n")
        f.write(f"QEMU 超时: {QEMU_TIMEOUT}s\n")
        f.write("=" * 60 + "\n\n")

        for arch in ["rv64", "la64"]:
            f.write(f"{'─' * 40}\n")
            f.write(f"  {arch.upper()}\n")
            f.write(f"{'─' * 40}\n")
            if arch in qemu_results:
                qr = qemu_results[arch]
                f.write(f"  QEMU rc: {qr['rc']}\n")
                f.write(f"  超时终止: {qr['timed_out']}\n")
            if arch in judge_results:
                jr = judge_results[arch]
                if jr["success"]:
                    f.write(f"\n{jr['stdout']}\n")
                else:
                    f.write(f"  评分错误: {jr.get('error', '')}\n")
                    if jr.get("stderr"):
                        f.write(f"  stderr: {jr['stderr']}\n")
            f.write("\n")

    log(f"  📄 summary.txt → {archive_dir}/")
    log(f"✅ 存档完成: {archive_dir}", GREEN)

    # ========== Final ==========
    print()
    print("=" * 60)
    print(f"  {BOLD}全量测试完成{RESET}")
    print(f"  存档目录: {archive_dir}")
    print(f"  完成时间: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print("=" * 60)

    if overall_ok:
        print(f"  {GREEN}✅ 所有步骤均已完成{RESET}")
    else:
        print(f"  {YELLOW}⚠️  部分步骤有异常，请检查上述输出{RESET}")

    sys.exit(0 if overall_ok else 1)


if __name__ == "__main__":
    main()
