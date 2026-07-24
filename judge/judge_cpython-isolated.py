#!/usr/bin/env python3
"""Judge the isolated CPython group from a full log or a sliced group body."""

import json
import re
import sys


START = "#### OS COMP TEST GROUP START cpython-isolated ####"
END = "#### OS COMP TEST GROUP END cpython-isolated ####"
RESULT = re.compile(
    r"\[CPYTHON\s+L\d+(?:-[A-Z0-9]+)?(?:\s+(PASS|FAIL))?\](.*)",
)


def judge_cpython(text: str) -> dict:
    has_start = START in text
    has_end = END in text
    if has_start != has_end:
        return {"error": "incomplete cpython-isolated group markers"}
    if has_start:
        text = text.split(START, 1)[1].split(END, 1)[0]

    all_count = 0
    pass_count = 0

    for line in text.splitlines():
        line = line.strip()
        match = RESULT.search(line)
        if not match:
            continue
        status = match.group(1)
        if status is None:
            trailing = re.search(r"\b(PASS|FAIL)\b", match.group(2))
            status = trailing.group(1) if trailing else None
        if status is None:
            continue
        all_count += 1
        if status == "PASS":
            pass_count += 1

    if all_count == 0:
        return {"error": "missing CPython PASS/FAIL results"}
    return {"all": all_count, "pass": pass_count}


if __name__ == "__main__":
    text = sys.stdin.read()
    result = judge_cpython(text)
    json.dump(result, sys.stdout)
