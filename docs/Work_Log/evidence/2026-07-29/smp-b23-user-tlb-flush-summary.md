# SMP B23 用户 PTE 锁外 shootdown 证据摘要

## 1. 证据对象

- 分支：`smp`
- 基线/HEAD：`1560ed6eaa817eaa6dfe98d111a7fd41d2eac90d`
  (`feat(smp): add user tlb synchronization foundation`)
- B23 状态：`pass` — 实现、维护性重构与冻结验证完成，维护者已批准提交
- 本地协作目录 `cc-codex/` 被 Git 忽略，不进入 GitHub。

## 2. 当前实现顺序

```text
AddressSpace::write
  -> 获取 VM 锁，MmuGather::begin(cached mask)
  -> UserMapper 执行 raw/no-flush PTE 写入
  -> MmuGather::record_change / retire_frame
  -> MmuGather::seal(&TlbContext)
  -> 释放 VM 锁
  -> TlbFlush::execute
  -> 本地失效或远端 USER_TLB_SYNC/ack
  -> observed[target].fetch_max(generation)
  -> drop 数据 frame / 页表 frame
```

它覆盖 unmap、mprotect、CoW、MAP_SHARED/anonymous/file fault、exec、OOM 回收和
zombie 页表释放。`ProcessInner.vm` 不暴露可变 guard；trap/clone/OOM/SHM 调用点
也不把 `task.inner`、`TASK_MANAGER` 或 `SHM_REGISTRY` 带过 ack 等待点。

相比首版 B23，当前实现删除了 `LockedAddressSpace`、`TlbPublication`、
`PendingUserTlb`、多层 commit 对象和重复退休队列。一个 `write()` 只有一个
`MmuGather`，维护者只需理解 `record_change -> seal -> execute`。

## 3. focused 用例的证明边界

`smp::user_tlb_retirement_waits_for_ack` 使用真实 `AddressSpace<PageTableImpl>` 和一页
用户映射：

1. CPU0/CPU1 都登记到同一 `TlbContext.cached_cpus`；
2. CPU1 在 kernel-only focused task 中关闭本地中断，记录 request 基线；
3. CPU0 通过 `AddressSpace::write()` 撤销该页，发布 request 后等待 CPU1；
4. CPU1 在尚不可能 ack 的窗口读取空闲 frame 数，确认与 unmap 前一致；
5. CPU1 开中断处理 IPI，CPU0 收到 ack 并返回；此时空闲 frame 数恰好增加 1。

该用例验证真实 PTE 撤销、request/ack 和 frame 退休的控制面闭环。它没有让普通
用户指令在 AP 上持续访问旧映射，因此不单独声称完成 victim 无 trap 的
stale-translation 硬件实验。

## 4. Docker/QEMU 环境

- image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- image created：`2026-05-10T08:46:16.065707166Z`
- mount：`/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`
- RV64/LA64 QEMU：`QEMU emulator version 10.0.2`
- rustc：`1.97.0-nightly (82bee9650 2026-05-09)`

## 5. 验证结果

### 5.1 重构前原型证据

首版 B23 曾完成双架构 normal build、`CORE_NUM=8 KTEST=smp` 和 `mask=0x003`
回归。该组结果用于确认协议原型，但源码结构已经重构，不能作为当前版本的最终 PASS。

### 5.2 重构中诊断构建

| Job | 架构 | 进程结果 | Runner 结果 | 采信范围 |
|-----|------|----------|-------------|----------|
| `agent-4a88be861024-r01-rv64-kernel-build` | RV64 | exit 0 | FAIL (`mutation_detected=true`) | 只证明当时快照可编译，不验收 |
| `agent-4a88be861024-r02-la64-kernel-build` | LA64 | exit 0 | FAIL (`mutation_detected=true`) | 只证明当时快照可编译，不验收 |

### 5.3 冻结源码门禁

| Job | 配置 | 状态 | 用时 | 关键事实 |
|-----|------|------|------|----------|
| `agent-683ce7d53fd1-r01-rv64-ktest` | RV64, 8 核, `KTEST=smp KREPEAT=1` | `pass` | 141.441 s | 16/16，online `0xff`，两项 user-TLB 用例通过 |
| `agent-683ce7d53fd1-r02-la64-ktest` | LA64, 8 核, `KTEST=smp KREPEAT=1` | `pass` | 141.155 s | 16/16，online `0xff`，两项 user-TLB 用例通过 |
| `agent-683ce7d53fd1-r03-rv64-preliminary` | RV64, 8 核, `mask=0x003` | `pass` | 340.363 s | 312/314，仅两组既有 `kill 10` |
| `agent-683ce7d53fd1-r04-la64-preliminary` | LA64, 8 核, `mask=0x003` | `pass` | 364.215 s | 308/314，既有 `test_brk` + `kill 10` |

四项进程退出码均为 0，无 timeout、panic、forbidden marker 或缺失完成标记；
测试前后均为：

- HEAD：`1560ed6eaa817eaa6dfe98d111a7fd41d2eac90d`
- status SHA-256：`be368587f2d62770d7e8a3c4d1aacfb51fa6f77a52d99abeb9509dc4bcd68b7e`
- tracked diff SHA-256：`02412e4c18b7f68dd630ba73a138a5c58ea77be98f7789e0ddacc9d225c1483b`
- untracked content SHA-256：`4fe2f1eb06884529e67bdaa7d378cbf6c61b639b733c3cbfce76010752646524`
- `mutation_detected=false`

preliminary recipe 本身包含对应架构 normal build，focused recipe 构建 ktest 内核；
因此没有再重复运行信息等价的独立 build。RV64 本轮 `test_pipe` 全部通过，失败集合
相较允许基线缩小；LA64 失败身份与既有基线完全一致。

验证后仅增加两处中文解释注释，不改变控制流或生成代码；冻结时 `os/` diff SHA-256
为 `94073ca0e421e163983facd80068ece4166e87db68b2245576876873739735f5`，注释后为
`2296045acbb452fa3bebe88120a1dcd6633507a73177b264973204ee197be3b9`。

## 6. DeepSeek 协作与人工裁决

DeepSeek/本地 Worker 负责冻结源码的机械构建、QEMU 运行、日志归纳和只读复核；
GPT/Codex 负责接口重构、并发正确性推理和最终采信。任何模型给出的 PASS 都必须同时
满足进程退出码、完成标记、源码指纹不变和日志中无 panic/timeout。

首轮诊断构建虽然 RV64/LA64 进程均退出 0，但 runner 检测到源码变化，因此明确拒绝
把它写成最终 PASS。这验证了协作协议的 fail-closed 行为。

最终只读审查 job `smp-b23-mmu-refactor-review-20260729` 用时 710.847 秒，进程退出 0、
`mutation_detected=false`。它确认 `record_change -> seal -> execute` 足够清晰且没有
阻塞性并发缺陷。GPT/Codex 采纳两项注释增强，但不采纳以下扩展：

1. 所谓 `seal()` 与 `activate_on()` 之间可观察旧 generation 的竞态不成立；
   `seal()` 推进 generation 时仍持 VM 锁，激活方只能在解锁后看到新代。
2. 删除 VMA 无关泛型、合并 OOM 架构副本和移除 `vm_mapping()` 别名不属于本次
   MMU 协议重构，避免把独立清理混入 B23。
3. `with_user_mapper()` 只解决 Rust 对 `vmas/page_table/mmu_gather` 的分字段借用，
   不承担 commit/flush 语义；保留它不会恢复旧的多层提交链。

## 7. 明确未验收范围

- 普通用户任务跨 CPU 运行与迁移；
- 用户 victim 在无 trap 窗口内持续访问 stale PTE 的硬件证据；
- LoongArch MM-owned ASID、epoch rollover 与跨核复用；
- RISC-V SBI RFENCE、ASID/range 精确 shootdown；
- cached CPU detach/lazy TLB；
- CLONE_VM 多线程高频 syscall 的 VM 锁竞争、IPI 目标数、ack 等待和 TLB refill 性能；
- process-wide exec/exit/signal 跨核停止与 uaccess 物理切片生命周期的最终审计。

上述范围不能由 CPU0-only 初赛回归或 kernel-only focused 用例外推为 PASS。
