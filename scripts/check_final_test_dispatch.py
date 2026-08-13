#!/usr/bin/env python3
"""Fail closed when final-test groups stop dispatching their official scripts."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "user/src/bin/test_runner/groups/catalog.rs"
EXECUTE = ROOT / "user/src/bin/test_runner/groups/execute.rs"


def require(source: str, needle: str, description: str) -> None:
    if needle not in source:
        raise SystemExit(f"missing {description}: {needle}")


def main() -> None:
    catalog = CATALOG.read_text()
    execute = EXECUTE.read_text()

    require(catalog, '("buildstorm", "buildstorm_testcode.sh")', "BuildStorm mapping")
    require(catalog, '("cagent", "cagent_testcode.sh")', "CAgent mapping")
    require(execute, 'format!("./{}\\0", script)', "catalog-selected command")

    forbidden = (
        'else if group == "buildstorm" || group == "cagent" {\n        format!',
        'BUILDSTORM_TOOLCHAIN ok',
        'BUILDSTORM_COMPILE mode=multi',
    )
    for marker in forbidden:
        if marker in execute:
            raise SystemExit(f"embedded BuildStorm dispatch returned: {marker}")

    print("final test dispatch contract: ok")


if __name__ == "__main__":
    main()
