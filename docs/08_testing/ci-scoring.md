---
title: "统一 CI 与 L5 评分"
category: testing
status: stable
last_update: 2026-07-25
code_paths:
  - ".github/workflows/ci.yml"
  - "scripts/score_test.py"
  - "scripts/full_test/runner.py"
  - "judge/run_parse.py"
---

# 统一 CI 与 L5 评分

`.github/workflows/ci.yml` 是 `develop` 与 `main` 唯一的 CI 定义：两分支 push、目标为两分支的 PR 和手动 dispatch 都运行同一条路径。手动 dispatch 的 `qemu_timeout` 直接成为 `QEMU_TIMEOUT`；默认值为 7200 秒。

## 执行顺序与失败处理

CI 采用 5 层顺序流水线，每层在独立的 Docker 容器中运行，使用 `docker compose up -d` / `docker compose exec` / `docker compose down` 模式：

| 层 | Job | 命令 | 超时 |
|----|-----|------|------|
| 0 | `toolchain-contracts` | 工具链验收合约（不变） | 5 min |
| 1 | `clippy` | `cargo clippy --features "board_rvqemu block_virt" -- -D warnings` | 15 min |
| 2 | `cargo-test` | `cargo test --features "board_rvqemu block_virt"` | 15 min |
| 3 | `ktest` | `make -C os rv64-ktest`（内核自检，无磁盘） | 15 min |
| 4 | `regression` | `make test ARCH=rv64 PROFILE=regression`（initramfs 仅） | 15 min |
| 5 | `comp` | `timeout 7200s make -f make/rv64.mk comp`（11 组，mask=0xFDF） | 350 min |

每层通过 `needs:` 保证顺序：前一层的测试全部通过后，下一层才启动。仅构建 RV64 架构（LA64 不在 CI 中运行，以加速反馈周期）。

### 各层详情

**Layer 1 (clippy)：** 运行 cargo clippy 的静态分析，`-D warnings` 将任何 warning 提升为 error。无 QEMU 依赖，纯 host-side 检查。

**Layer 2 (cargo test)：** 运行 `mango-kernel-core` 的纯逻辑单元测试（L1 测试集），在 host 上执行，无需 QEMU。

**Layer 3 (ktest)：** 构建内核并启动 QEMU 执行内核自检（L3 测试集），使用 initramfs 而非磁盘，测试完成后 shutdown。输出 TAP 格式测试结果。

**Layer 4 (regression)：** 构建内核（regression profile），启动 QEMU 执行用户态回归测试（L4 测试集），使用 initramfs 而非磁盘。检测串口输出中的 `[L4 REGRESSION RESULT: PASS]` 字样。

**Layer 5 (comp)：** 完整的竞赛场景——
1. 下载/还原缓存官方测试镜像 `sdcard-rv.img`
2. 注入 `os_test.conf`（mask=0xFDF，排除 unixbench/cpython）
3. 构建内核及用户程序
4. 运行 `make -f make/rv64.mk comp`，QEMU 完成 11 组测试后 shutdown
5. comp 目标直接根据测试结果返回退出码（CI 由此判断通过/失败）

> ⚠️ 不同于旧版 CI，comp 层不再运行 `python3 scripts/run_full_test.py --serial`，也不生成评分 JSON 或上传 artifact。测试通过与否完全由 `make comp` 的退出码决定。

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
