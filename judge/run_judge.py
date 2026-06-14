#!/usr/bin/env python3
"""
用法: python3 run_judge.py <os_serial_out.txt>

读取 os_serial_out_xxx.txt，切割其中的
  #### OS COMP TEST GROUP START xxx-yyy ####
  ... 输出 ...
  #### OS COMP TEST GROUP END xxx-yyy ####
并把每段输出喂给同目录下的 judge_xxx-yyy.py，汇总打印结果。
"""
import re, json, math, subprocess, os, sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))

def _ltp_adjust(name, raw):
    """LTP raw([0,10000]) → score([0,500]): 500*log10(1+9*raw/10000)"""
    if "ltp" in name.lower():
        clipped = max(0.0, min(float(raw), 10000.0))
        return 500.0 * math.log10(1 + 9 * clipped / 10000.0)
    return float(raw)

def find_judges():
    """返回 {group: script_path}"""
    judges = {}
    for f in sorted(os.listdir(SCRIPT_DIR)):
        if f.startswith("judge_") and f.endswith((".py", ".sh")):
            name = f.removeprefix("judge_").rsplit(".", 1)[0]
            judges[name] = os.path.join(SCRIPT_DIR, f)
    return judges

def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <os_serial_out.txt>")
        sys.exit(1)

    logfile = sys.argv[1]
    judges = find_judges()
    print(f"Found {len(judges)} judge scripts: {list(judges.keys())}")

    with open(logfile, "r", encoding="utf-8", errors="ignore") as f:
        lines = f.readlines()

    start_re = re.compile(r"#### OS COMP TEST GROUP START ([a-zA-Z0-9_-]+) ####")
    end_str   = "#### OS COMP TEST GROUP END"

    current_group = None
    current_lines = []
    results = {}

    for line in lines:
        m = start_re.search(line)
        if m:
            current_group = m.group(1)
            current_lines = []
            continue

        if end_str in line and current_group:
            # 喂给对应 judge
            if current_group in judges:
                try:
                    p = subprocess.Popen(
                        [sys.executable, judges[current_group]],
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                    )
                    p.stdin.write("".join(current_lines).encode())
                    p.stdin.close()
                    out = p.stdout.read().decode()
                    results[current_group] = json.loads(out)
                except Exception as e:
                    results[current_group] = {"error": str(e)}
            else:
                results[current_group] = {"error": f"no judge for {current_group}"}
            current_group = None
            current_lines = []
            continue

        if current_group:
            current_lines.append(line)

    # 没有 START 标记的组也跑一下（空输入）
    for g, j in judges.items():
        if g not in results:
            try:
                p = subprocess.Popen(
                    [sys.executable, j],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                )
                p.stdin.close()
                out = p.stdout.read().decode()
                results[g] = json.loads(out)
            except Exception as e:
                results[g] = {"error": str(e)}

    # 打印结果
    print()
    print("=" * 60)
    print(f"{'GROUP':<28} {'PASS':>5} {'ALL':>5}  RATE")
    print("=" * 60)
    total_all = 0
    total_pass = 0
    for name in sorted(results.keys()):
        r = results[name]
        if "error" in r:
            print(f"  {name:<26}  ERROR: {r['error']}")
            continue
        a = r.get("all", 0)
        p = r.get("pass", 0)
        total_all += a
        total_pass += _ltp_adjust(name, p)
        pct = f"{p/a*100:.0f}%" if a > 0 else "N/A"
        print(f"  {name:<26}  {p:>5} {a:>5}  {pct}")
    print("=" * 60)
    print(f"  {'TOTAL':<26}  {total_pass:>7.1f} {total_all:>5}")
    print(f"  {'SCORE':<26}  {total_pass:>7.1f}")

if __name__ == "__main__":
    main()
