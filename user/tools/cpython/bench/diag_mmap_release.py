#!/usr/bin/env python3
"""Measure anonymous private mmap teardown after every page is resident."""

import json
import mmap
import os
import resource
import time


STATS_ROOT = "/sys/kernel/stats"


def stats_write(name, value):
    with open(os.path.join(STATS_ROOT, name), "w", encoding="ascii") as control:
        control.write(str(value) + "\n")


def main():
    size_mib = int(os.environ.get("MANGO_MMAP_MIB", "1"))
    size = size_mib * 1024 * 1024
    mapping = mmap.mmap(
        -1,
        size,
        flags=mmap.MAP_PRIVATE,
        prot=mmap.PROT_READ | mmap.PROT_WRITE,
    )
    for offset in range(0, size, 4096):
        mapping[offset] = 1

    stats_write("stats_on", 0)
    stats_write("reset", 1)
    usage_before = resource.getrusage(resource.RUSAGE_SELF)
    stats_write("stats_on", 1)
    started_ns = time.perf_counter_ns()
    mapping.close()
    elapsed_ns = time.perf_counter_ns() - started_ns
    usage_after = resource.getrusage(resource.RUSAGE_SELF)
    stats_write("stats_on", 0)

    print(
        "CPYTHON_DIAG_JSON "
        + json.dumps(
            {
                "type": "mmap_release",
                "size_mib": size_mib,
                "pages": size // 4096,
                "elapsed_ns": elapsed_ns,
                "user_seconds": usage_after.ru_utime - usage_before.ru_utime,
                "sys_seconds": usage_after.ru_stime - usage_before.ru_stime,
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        flush=True,
    )


if __name__ == "__main__":
    main()
