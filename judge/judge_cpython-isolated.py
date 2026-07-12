#!/usr/bin/env python3
"""Judge for CPython isolated test group.

Parses serial output between:
    #### OS COMP TEST GROUP START cpython-isolated ####
    #### OS COMP TEST GROUP END cpython-isolated ####

Counts [CPYTHON Lx PASS] and [CPYTHON Lx FAIL] markers.
"""

import json
import re
import sys


def judge_cpython(text: str) -> dict:
    if "#### OS COMP TEST GROUP START cpython-isolated ####" not in text:
        return {"error": "missing START marker"}
    if "#### OS COMP TEST GROUP END cpython-isolated ####" not in text:
        return {"error": "missing END marker (script died mid-way)"}

    all_count = 0
    pass_count = 0

    for line in text.splitlines():
        line = line.strip()
        # Match lines like: [CPYTHON L5] test: arithmetic PASS
        # or: [CPYTHON L5] test: arithmetic FAIL (assertion error)
        m = re.search(r"\[CPYTHON\s+L\d+\].*\b(PASS|FAIL)\b", line)
        if m:
            all_count += 1
            if m.group(1) == "PASS":
                pass_count += 1

    return {"all": all_count, "pass": pass_count}


if __name__ == "__main__":
    text = sys.stdin.read()
    result = judge_cpython(text)
    json.dump(result, sys.stdout)
