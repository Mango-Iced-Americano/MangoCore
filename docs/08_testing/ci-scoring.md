---
title: "统一 CI 与 L5 评分"
category: testing
status: stable
last_update: 2026-07-24
code_paths:
  - ".github/workflows/ci.yml"
  - "scripts/score_test.py"
  - "scripts/full_test/runner.py"
  - "judge/run_parse.py"
---

# 统一 CI 与 L5 评分

`.github/workflows/ci.yml` 是 `develop` 与 `main` 唯一的 CI 定义：两分支 push、目标为两分支的 PR 和手动 dispatch 都运行同一条路径。手动 dispatch 的 `qemu_timeout` 直接成为 `QEMU_TIMEOUT`；默认值为每个架构 7200 秒。

## 执行顺序与失败处理

1. 使用 `docker compose` 拉取 `os-dev`，下载并缓存官方双架构测试镜像。
2. 在容器内串行运行 `make all MODE=release`（RV64 后 LA64），再分别将 CI 配置注入两个镜像。
3. 在容器内执行 `make lint`，随后运行 `python3 scripts/run_full_test.py --serial`。full-test runner 为每个架构写入 `testresult/archive_*/<arch>/qemu.log`，并对超时、非零退出、致命内核签名和缺失终止标记 fail-closed。
4. 即使 QEMU runner 失败，评分步骤仍会尝试处理两份原始日志；最终门禁要求 QEMU runner 与两个评分命令都成功。因此失败日志和 JSON 一定会作为 artifact 尽可能保留。

产物 `ci-qemu-logs-and-scores` 包含完整 `testresult/`（原始 QEMU、build、judge 日志）以及 `testresult/scores/rv64.json`、`la64.json`。它也是失败排查的唯一输入，无需重跑 QEMU。

## 选择的测试组与掩码

评分的 11 个组均对 musl 和 glibc 运行：basic、busybox、lua、libctest、iozone、libcbench、lmbench、iperf、netperf、cyclictest 和 ltp。unixbench 与 cpython 不评分也不运行。

当前 `TEST_GROUPS` 的 unixbench 位为 5、ltp 位为 11、cpython 位为 12；因此实际选择这 11 组的掩码是 `0xFDF`。常见的 `0x7FF` 仅在“unixbench 是 bit 11”这一过时编号下成立；在当前 runner 中它会运行 unixbench 而遗漏 ltp，不能满足 CI 合同。

## 日志解析和评分公式

`scripts/score_test.py` 不自行猜测日志格式。它调用 `judge/run_parse.py`，后者依据：

```text
#### OS COMP TEST GROUP START <group>-<libc> ####
... test output ...
#### OS COMP TEST GROUP END <group>-<libc> ####
```

分段并运行同一批 `judge_*.py`。列表型 judge 输出中每项 `score/pass > 0` 视为一个通过项；对象型输出直接使用 `pass` 与 `all`。未出现、空或不可解析的组记为 0/0，必然不能通过。

对每个架构有 22 个等权的 `(group, libc)` 变体。变体通过率为 `pass / (pass + fail)`，总分为：

```text
score = 100 / 22 × Σ variant_pass_rate
```

这避免 LTP 的大量断言数压过其余组，同时 JSON 仍保留每个变体的实际 pass/fail 数。basic 与 busybox 的每个 libc 变体必须 `fail == 0` 且至少执行一个测试；其余九组的每个 libc 变体必须至少执行一个测试且通过率不低于 90%。任一架构的任一变体不满足门槛即令该架构 JSON 的 `passed` 为 `false`，CI 因此失败。

输出形状为：

```json
{
  "arch": "rv64",
  "groups": {
    "basic": {"musl": {"pass": 12, "fail": 0}, "glibc": {"pass": 12, "fail": 0}}
  },
  "score": 100.0,
  "passed": true
}
```
