# SMP B30 `getcpu()` 真实逻辑 CPU 证据摘要

## 结论

状态：`pass`。

B30 将 `getcpu()` 从固定 CPU0 占位实现改为返回当前连续逻辑 CPU 编号，并用 B29 已有的
真实用户迁移闭环验证 CPU0 → `sched_yield` → CPU1 的两次查询结果。RV64/LA64 8 核
focused 均为 21/21 PASS，双架构 `mask=0x003` 初赛门禁也没有退化。

本证据不代表 affinity、普通用户任务默认全核调度、NUMA、AP timer 或 FS/net/driver 多核
安全已经完成。

## 被测源码

- worktree：`/home/lzm/projects/MangoCore-smp-integration-20260725`
- branch：`smp`
- HEAD：`bc698ade5cea7c0e0aad567cd90f72b78b7ddcf4`
- 功能源码 diff SHA-256：
  `360bcbfddfd38854e0fc5f0102948b8a6d8586dcf0251539f33ec632f4997ee6`
- 功能修改：
  - `os/src/syscall/process/ids.rs`
  - `os/src/kernel_tests/smp.rs`

四次测试的 source-before/source-after HEAD、tracked diff 和 untracked content 指纹完全一致，
包装器均报告 `mutation_detected=false`。

## 生产语义

1. `smp::cpu_id()` 返回 scheduler/PerCpu 使用的 `0..N-1` 逻辑编号，不暴露硬件 hart/CoreID。
2. syscall 在写用户指针前只采样一次 CPU，两个可选输出描述同一次调用。
3. `cpu == NULL` 或 `node == NULL` 时分别忽略对应输出；tcache 保持 ABI 兼容并忽略。
4. MangoCore 没有 NUMA，node 返回 0；这与 CPU 编号无关。
5. 路径不持锁、不等待、不主动调度，也不改变任务 affinity 或发布策略。

## 动态反假通过

双架构用户探针保留 getpid/yield/exit，并增加两次 getcpu：

1. 任务发布到 CPU0，第一次 getcpu 必须成功并写入 0；从 CPU1 起跑会立即失败。
2. `sched_yield` 必须返回成功；源任务随后在 idle 栈完成 B29 的一次性迁移交接。
3. CPU1 恢复同一 syscall 栈后，第二次 getcpu 必须成功并写入 1。
4. 若 getcpu 仍固定返回 0、任务未迁移、迁到错误 CPU 或任一 syscall 失败，探针均 exit(1)，
   ktest 的 wait/reap 会把非零 status 判为失败。

探针只使用 16 字节已映射用户栈，代码页仍是装载期 RW、发布前 RX；不进入共享 FS、网络、
VirtIO、console 或设备路径，也没有新增生产调试字段。

## 只读审查

- job：`smp-b30-getcpu-review-001`
- 耗时：260.480 秒
- exit：0
- mutation：false
- 结果：无 blocker；采纳 yield 返回值显式检查。

审查报告有一处架构描述错误：LoongArch `ld.w` 是符号扩展，`ld.wu` 才是零扩展。当前只读取
CPU 0/1，因此不影响比较结果。没有采纳 exit 前恢复 16 字节用户栈的建议，因为 exit 不返回。

## 双架构 focused

| 架构 | child job | 耗时 | exit | online | TAP | 新语义 |
|---|---|---:|---:|---:|---:|---|
| RV64 | `agent-6e8ecdc6373e-r01-rv64-ktest` | 134.104s | 0 | `0xff` | 21/21 | PASS |
| LA64 | `agent-6e8ecdc6373e-r02-la64-ktest` | 136.046s | 0 | `0xff` | 21/21 | PASS |

两份日志都明确包含：

```text
ok 20 smp::user_task_migrates_on_yield
[KTEST RESULT: PASS]
```

没有 panic、fatal trap、TLB timeout、owner invariant、missing marker 或 source mutation。

## 初赛非回归

| 架构 | child job | 耗时 | exit | judge | 精确接受失败集合 |
|---|---|---:|---:|---:|---|
| RV64 | `agent-6e8ecdc6373e-r03-rv64-preliminary` | 330.627s | 0 | 312/314 | musl/glibc `busybox kill 10` 各 0/1 |
| LA64 | `agent-6e8ecdc6373e-r04-la64-preliminary` | 344.362s | 0 | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 |

两架构均为 `configured=8`、`online_mask=0xff`、`mask=0x003`，四个 basic/busybox END
完整。失败身份与既有接受基线一致，没有换位；原始 judge 分数没有被模型摘要或 semantic
归一化覆盖。

## 清理与证据边界

- 没有 68 字节用户工具桩、临时测试源码、独立 getcpu 用户 ELF、调试计数器、`.orig`、
  `.rej` 或新增 W+X 路径残留。
- 原始 prompt、DeepSeek 输出、stdout/stderr、runner manifest 留在本地忽略的
  `cc-codex/runtime/jobs/`，不上传 GitHub。
- 初赛只证明 getcpu 改动没有破坏当前 CPU0 普通用户路径；CPU1 的 getcpu 语义由 hermetic
  focused 探针直接证明，不能据此外推共享子系统已经适合普通任务跨核运行。
