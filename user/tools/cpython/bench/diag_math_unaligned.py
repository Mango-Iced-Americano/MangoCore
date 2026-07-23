#!/usr/bin/env python3
"""Attribute LoongArch unaligned traps to individual libm entry points."""

import json
import math
import os
import resource
import time


STATS_ROOT = "/sys/kernel/stats"


def stats_write(name, value):
    with open(os.path.join(STATS_ROOT, name), "w", encoding="ascii") as control:
        control.write(str(value) + "\n")


def syscall_counters():
    counters = {}
    with open(os.path.join(STATS_ROOT, "syscall"), encoding="ascii") as source:
        for line in source:
            key, separator, value = line.strip().partition("=")
            if separator:
                counters[key] = int(value)
    return counters


def main():
    loops = int(os.environ.get("MANGO_MATH_LOOPS", "20000"))
    functions = (
        ("baseline", lambda i: i * 0.0001),
        ("sqrt", lambda i: math.sqrt((i * 0.0001) ** 2 + 1.0)),
        ("sin", lambda i: math.sin(i * 0.0001)),
        ("cos", lambda i: math.cos(i * 0.00007)),
        ("log", lambda i: math.log(1.0 + (i * 0.0001) ** 2)),
        ("pow", lambda i: math.pow(1.0 + (i * 0.0001) ** 2, 0.25)),
        ("exp", lambda i: math.exp(i * 0.000001)),
    )
    stats_write("stats_on", 0)
    stats_write("profile", "core")
    for name, function in functions:
        stats_write("reset", 1)
        usage_before = resource.getrusage(resource.RUSAGE_SELF)
        stats_write("stats_on", 1)
        started_ns = time.perf_counter_ns()
        result = 0.0
        for index in range(1, loops + 1):
            result += function(index)
        elapsed_ns = time.perf_counter_ns() - started_ns
        usage_after = resource.getrusage(resource.RUSAGE_SELF)
        stats_write("stats_on", 0)
        counters = syscall_counters()
        print(
            "CPYTHON_DIAG_JSON "
            + json.dumps(
                {
                    "type": "math_unaligned",
                    "function": name,
                    "loops": loops,
                    "elapsed_ns": elapsed_ns,
                    "user_seconds": usage_after.ru_utime - usage_before.ru_utime,
                    "sys_seconds": usage_after.ru_stime - usage_before.ru_stime,
                    "result": result,
                    "counters": {
                        key: value
                        for key, value in counters.items()
                        if key.startswith("user_unaligned_")
                    },
                },
                sort_keys=True,
                separators=(",", ":"),
            ),
            flush=True,
        )


if __name__ == "__main__":
    main()
