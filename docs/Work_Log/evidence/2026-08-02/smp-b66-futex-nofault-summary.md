---
title: "B66 futex nofault 原子注册证据"
date: 2026-08-02
status: partial
phase: B66
---

# B66 futex nofault 原子注册证据

## 1. 验收范围

B66 只验收 WAIT/WAIT_BITSET/waitv 的注册锁序与线性化：faultable 用户读取、key 解析和
waiter 分配位于 `FutexTable` 锁外；最后一次 backing/PTE/word 比较通过 VM try-lock 做
nofault 读取，并与 waiter 发布处于同一个 table 临界区。内部 Retry 只在发布前返回。

阶段状态为 `partial`：生产实现、静态锁协议、双架构 8 核构建与 focused futex LTP 已完成；
精确 compare/enqueue 并发 wake、持续 VM 锁竞争和比较后 remap 尚未专项动态复现。

## 2. 环境与源码指纹

- Worktree: `/home/lzm/projects/MangoCore-smp-integration-20260725`
- Branch/基线: `smp` / `da6ee001c243f7ec42f4e9eb7d0525fdc845623c`
- Docker container: `mangocore-smp-integration-20260725-os-dev-1`
- Image: `zhouzhouyi/os-contest:20260510`
- Image ID:
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest:
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- QEMU: RV64 10.0.2 / LA64 10.0.2
- 冻结 source tracked diff SHA-256:
  `2af90af7130177e8525eff3b362cb4ea9ab52cf6ff170667668aa7e92dca3c37`
- 冻结 status SHA-256:
  `3e99b909b7dc16b0a1e3134f008109f831bfe90a21dac61c556f9403ac182384`
- 四项 recipe 的 source-before/source-after 指纹均一致。

## 3. 静态协议证明

1. syscall 先用 faultable `UserPtr::read()` 读取并比较期望值，再解析当前 private/shared
   key；此时不持有 futex table。
2. 注册 helper 在取得 table 锁前分配 `Arc<FutexWaiter>`、clone 当前进程 VM，避免临界区
   获取 `process.inner` 或分配内存。
3. table 锁内只调用 `AddressSpace::try_read()`；VM 锁忙立即产生 Retry，不在外层自旋锁
   内等待。
4. nofault 路径复核 shared backing `Arc::ptr_eq`、VMA resident/PTE 一致性、PTE 读权限，
   再做一次对齐 u32 volatile read；不能只用可复用 PPN 代表对象身份。
5. word 匹配后不释放 table，直接 enqueue waiter。因此状态写+wake 若先发生，locked read
   会观察到新值；wait 若先比较并发布，wake 随后会在同一 table 下找到 waiter。
6. Retry 只可能在任何 waiter 发布前返回；syscall 释放 table 后重做 faultable 读取和完整
   key 解析，不能沿用旧 shared backing，也不向用户返回伪 errno。
7. WAIT 的相对 timeout 只转换一次绝对 deadline。waitv 描述符只快照一次，每次 Retry 重读
   全部 word、重算全部 key，并在一把 table 锁下原子发布全组 waiter。
8. 入队后的第三次用户读取已删除；恢复只读取 waiter 的 `woken`/current-key 权威状态。

## 4. Docker 构建与 QEMU 结果

| 门禁 | DeepSeek child | 配置 | 结果 |
|------|----------------|------|------|
| RV64 kernel build | `agent-c8ad864d73cd-r01-rv64-kernel-build` | `CORE_NUM=8` | PASS，exit 0，128.379s |
| LA64 kernel build | `agent-c8ad864d73cd-r02-la64-kernel-build` | `CORE_NUM=8` | PASS，exit 0，138.864s |
| RV64 futex LTP | `agent-c8ad864d73cd-r03-rv64-futex-ltp` | `CORE_NUM=8` | PASS，305.400s，20 PASS / 6 SKIP / 0 FAIL |
| LA64 futex LTP | `agent-c8ad864d73cd-r04-la64-futex-ltp` | `CORE_NUM=8` | PASS，303.816s，20 PASS / 6 SKIP / 0 FAIL |

两架构均打印 `online_mask=0xff`；RV64 的 boot hardware ID 为 7，LA64 为 0，均无
panic、fatal、timeout 或 forbidden marker。每架构的实际 PASS 项为
`futex_cmp_requeue02`、`futex_wait01..05`、`futex_wait_bitset01`、
`futex_wake01..03`，musl/glibc 各执行一次；`futex_waitv01..03` 在两套 libc 下均因
用例要求 Linux 5.16、当前 uname 为 5.10 而 SKIP。

DeepSeek 总任务：

- `smp-b66-nofault-design-r1`
- `smp-b66-build-validation-r1`

GPT/Codex 独立解析原始日志后纠正了 DeepSeek 报告中误列的
`futex_cmp_requeue01`、`futex_requeue01`、`futex_wake_bitset01`；最终计数与用例清单
以上述原始日志为准。

## 5. 明确未验收

- **NOT RUN:** 专门放大“最后 locked compare 与并发状态写+wake”的精确交错；当前以唯一
  table 线性化点做静态证明。
- **NOT RUN:** 另一 CPU 长时间持有 VM 锁时连续 Retry 的公平性、livelock 与性能。
- **NOT RUN:** nofault 比较后、waiter 发布前后并发 unmap/remap 的专项映射生命周期压力。
- **NOT RUN:** B65 已记录的 `force_swap_out()`/truncate backing false-negative 和 pin
  对内存回收的量化影响。
- 初赛 `mask=0x003` 未重复执行；B66 使用直接覆盖变更面的双架构 futex LTP，不能把历史
  初赛账本记成本批新证据。

## 6. 上游对照

- Linux `futex_wait_setup()`：
  https://github.com/torvalds/linux/blob/master/kernel/futex/waitwake.c
- Linux futex locked value helper：
  https://github.com/torvalds/linux/blob/master/kernel/futex/futex.h

Linux 同样在 bucket 锁下做 nofault 值读取，读取失败时解锁、fault-in，并为 shared futex
重新计算 key。MangoCore 额外使用 VM try-lock，避免自身地址空间锁在 table 自旋锁内等待。
