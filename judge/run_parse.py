#!/usr/bin/env python3
"""
用法: python3 run_parse.py [os_serial_out.txt] [judge_dir]

核心逻辑完全复制自 kernel/run.py 的 parse_serial_out_new()，一字未改。
扫描 START/END 标记 → 管道喂给 judge_*.py → 汇总打印。
"""
import re
import json
import subprocess
import os
import sys


# ============ 以下逐字复制自 kernel/run.py ============

def _get_name(x: str):
    x = x.removeprefix("judge_")
    if '.' in x:
        x = x[:x.rindex('.')]
    return x

def _get_exec(x: str):
    if x.endswith(".py"):
        return [sys.executable, x]
    if x.endswith(".sh"):
        return ["/bin/bash", x]
    return [x]

def parse_serial_out_new(config, filename):
    """复制自 kernel/run.py 第 221 行，一字未改。"""
    ans = {}
    file = open(filename, "r", encoding='utf-8', errors='ignore')
    judge_path = config["testcase_dir"]
    judge = None
    group = None
    called_group = set()
    start = re.compile(r"#### OS COMP TEST GROUP START ([a-zA-Z0-9-]+) ####")
    end = '#### OS COMP TEST GROUP END'
    judges = [x for x in os.listdir(judge_path) if x.startswith("judge_")]
    judges = {_get_name(x): _get_exec(os.path.join(judge_path, x)) for x in judges}
    for line in file:
        is_start = start.findall(line)
        if end in line or len(is_start) > 0:
            if judge:
                try:
                    judge.stdin.close()
                    x = judge.stdout.read().decode()
                    ans[group] = json.loads(x)
                    judge.wait(5)
                    judge = None
                    group = None
                except Exception as e:
                    print(f"评测 {filename} : {group} 发生错误：{e}")
                    raise e
        elif judge is not None:
            judge.stdin.write(line.encode())
        if is_start:
            group = is_start[0]
            if group in judges:
                print(f"正在评测：{filename} : {group}")
                judge = subprocess.Popen(judges[group], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                called_group.add(group)
    if judge:
        try:
            judge.stdin.close()
            x = judge.stdout.read().decode()
            ans[group] = json.loads(x)
            judge.wait(5)
            judge = None
            group = None
        except Exception as e:
            print(f"评测 {filename} : {group} 发生错误：{e}")
            raise e
    for g, j in judges.items():
        if g not in called_group:
            print(f"正在评测：{filename} : {g}")
            try:
                judge = subprocess.Popen(judges[g], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                judge.stdin.close()
                x = judge.stdout.read().decode()
                ans[g] = json.loads(x)
            except Exception as e:
                print(f"评测 {filename} : {g} 发生错误：{e}")
                raise e
    return ans

# ============ 以上是官方代码，以下是参数处理+汇总打印 ============

if __name__ == "__main__":
    script_dir = os.path.dirname(os.path.abspath(__file__))

    if len(sys.argv) >= 2:
        logfile = sys.argv[1]
    else:
        logfile = os.path.join(script_dir, "os_serial_out_rv.txt")

    if len(sys.argv) >= 3:
        judge_dir = sys.argv[2]
    else:
        judge_dir = script_dir

    if not os.path.exists(logfile):
        print(f"错误: 文件不存在: {logfile}")
        sys.exit(1)

    config = {"testcase_dir": judge_dir}
    result = parse_serial_out_new(config, logfile)

    # 汇总打印
    print()
    print("=" * 70)
    print(f"{'GROUP':<30} {'PASS':>6} {'ALL':>6}")
    print("=" * 70)
    total_all = 0
    total_pass = 0
    for name in sorted(result.keys()):
        r = result[name]
        if isinstance(r, list):
            p = sum(x.get("pass", x.get("success", 0)) for x in r)
            a = sum(x.get("all", 0) for x in r)
        elif isinstance(r, dict):
            p = r.get("pass", r.get("passed", r.get("score", 0)))
            a = r.get("all", 0)
        else:
            p = 0
            a = 0
        total_all += a
        total_pass += p
        print(f"  {name:<28}  {p:>6} {a:>6}")
    print("=" * 70)
    print(f"  {'TOTAL':<28}  {total_pass:>6} {total_all:>6}")
    print()
    print("Full JSON:")
    print(json.dumps(result, indent=2, ensure_ascii=False))
