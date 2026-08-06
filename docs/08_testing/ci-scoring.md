---
title: "统一 CI 与 L5 评分"
category: testing
status: stable
last_update: 2026-08-03
code_paths:
  - ".github/workflows/ci.yml"
  - "scripts/score_test.py"
  - "scripts/full_test/runner.py"
  - "judge/run_parse.py"
---

# 统一 CI 与 L5 评分

`.github/workflows/ci.yml` 是统一 CI 定义：任意分支的 push、PR 和手动 dispatch 都运行同一条路径。手动 dispatch 的 `qemu_timeout` 直接成为 `QEMU_TIMEOUT`；默认值为 7200 秒。

## 执行顺序与失败处理

CI 先执行工具链合同，随后各测试 job 可以独立运行；同一个 job 内始终按照 RV64 → LA64 串行执行，避免共享工具链和生成状态竞态。所有构建和测试都使用 `docker compose up -d` / `docker compose exec` / `docker compose down` 模式：

| 层 | Job | 命令 | 超时 |
|----|-----|------|------|
| 0 | `toolchain-contracts` | Make/Rustup/source-purity 合同；失败会被记录，但不阻止其余 job 提供诊断 | 5 min |
| 1 | `clippy` | 双架构 board/block feature，RV64 → LA64 | 20 min |
| 2 | `cargo-test` | host `mango-kernel-core` 单元测试，两次架构配置串行执行 | 20 min |
| 3 | `ktest` | `rv64-ktest` → `la64-ktest` | 30 min |
| 4 | `regression` | 双架构 regression QEMU | 30 min |
| 5a | `comp-rv` | RV64 比赛镜像，mask=`0xFDF` | 350 min |
| 5b | `comp-la` | LA64 比赛镜像，mask=`0xFDF` | 350 min |

`clippy`、`cargo-test`、`ktest`、`regression`、`comp-rv` 和 `comp-la` 都只依赖 `toolchain-contracts`，彼此没有串行依赖。这里的“层”表示验证深度，不表示所有 job 在 GitHub runner 上依次排队。

### 各层详情

**Layer 1 (clippy)：** 在 Docker 内运行双架构 cargo clippy。SMP 分支在 HAL v2 融合前仍使用 `board_rvqemu`/`board_laqemu` feature；不能提前使用 develop 的 `boot_la_qemu` 名称。

**Layer 2 (cargo test)：** 在 Docker 内以 host target 运行 `mango-kernel-core` 的纯逻辑单元测试，无需 QEMU。

**Layer 3 (ktest)：** 构建内核并启动 QEMU 执行内核自检（L3 测试集），使用 initramfs 而非磁盘，测试完成后 shutdown。输出 TAP 格式测试结果。

**Layer 4 (regression)：** 构建内核（regression profile），启动 QEMU 执行用户态回归测试（L4 测试集），使用 initramfs 而非磁盘。检测串口输出中的 `[L4 REGRESSION RESULT: PASS]` 字样。

**Layer 5 (comp)：** RV64 与 LA64 分为两个独立 job，分别执行完整竞赛场景——

1. 下载/还原缓存官方测试镜像 `sdcard-rv.img`
2. 注入 `os_test_ci.conf`（mask=0xFDF，排除 unixbench/cpython）
3. 构建内核及用户程序
4. 运行相应架构的 `derived-comp`，QEMU 完成测试后 shutdown
5. 保存原始串口日志，在容器内生成评分 JSON，并以 `--table` 打印人类可读的 pass/total 表格

CI 当前没有上传评分 artifact；JSON 位于 job 容器的 `/tmp`，表格进入 Actions 日志。`Score` 步骤用于汇总展示，QEMU/`derived-comp` 的退出状态仍是运行阶段的主要门禁。

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

对每个架构最多有 22 个等权的 `(group, libc)` 变体。变体通过率为 `pass / (pass + fail)`。judge 列表中 `all=0` 的占位项不进入统计；显示分数只平均实际产生测试数据的变体：

```text
R = { variant | variant_total > 0 }
score = 0                              if R is empty
score = 100 / |R| × Σ rate(variant)    otherwise
```

这避免 LTP 的大量断言数压过其余组，也避免无数据占位项扭曲展示分数。缺失变体虽然不进入显示分数的分母，但仍会使 `passed=false`，不能借此绕过门禁。basic 与 busybox 的每个 libc 变体必须 `fail == 0` 且至少执行一个测试；其余九组的每个 libc 变体必须至少执行一个测试且通过率不低于 90%。JSON 保留每个变体的实际 pass/fail 数；传入 `--table` 时 stdout 改为简洁表格，JSON 文件仍照常生成。

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
