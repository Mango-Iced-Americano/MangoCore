---
title: "B65 shared futex 稳定 backing key 证据"
date: 2026-08-02
status: partial
phase: B65
---

# B65 shared futex 稳定 backing key 证据

## 1. 验收范围

B65 只验收 process-shared futex key 的对象身份与生命周期：raw PPN + offset 被替换为
backing `Arc<FrameTracker>` 对象身份 + 页内偏移；每个非空 shared queue 保留一份 backing
pin。它不宣称已经解决 table 锁内 faultable uaccess，也不宣称强制换出或 truncate 后仍保持
同一 backing identity。

阶段状态为 `partial`：生产代码、静态生命周期证明、双架构 8 核构建与 focused LTP 均完成，
但精确物理页复用 ABA 动态竞态未专门构造。

## 2. 环境与源码指纹

- Worktree: `/home/lzm/projects/MangoCore-smp-integration-20260725`
- Branch/基线: `smp` / `284ea47f79acdc727233bd4ae614ee6080c9cb46`
- Docker container: `mangocore-smp-integration-20260725-os-dev-1`
- Image: `zhouzhouyi/os-contest:20260510`
- Image ID: `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest:
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- QEMU: RV64 10.0.2 / LA64 10.0.2
- 最终 tracked diff SHA-256:
  `3696a7d77a09a04d234412f08db04d314c351e208aa337c0d8011b00532a4630`
- 两个 focused child 的 source-before/source-after 指纹一致，mutation=false。

## 3. 静态证明

1. `AddressSpace::futex_shared_backing()` 在同一 VM 锁保护期内确认 VMA 为 shared、clone
   resident frame，并验证 `backing.ppn == PTE.ppn`。
2. file-backed `MAP_SHARED`、anonymous shared fork、SysV SHM 与同一文件页多次 mmap 的
   两侧 VMA 均共享同一 backing Arc，而不是仅恰好共享 PPN。
3. syscall/`clear_child_tid` 先持有 `SharedFutexKey`，释放 VM 锁后才获取 futex table；没有
   `AddressSpace -> FutexTable` 嵌套锁。
4. 内部 `QueueKey` 为 `(Arc::as_ptr(), page_offset)`；对应非空 `FutexQueue` 持有一份
   `backing_pin`，所以整数地址在队列存活期间不会悬空复用。
5. requeue 先创建或验证目标队列 pin，再更新 waiter current key，最后发布到目标队列；
   timeout/signal/waitv 仍按准确 Arc waiter 身份清理。
6. waiter 的 backing identity 与 offset 虽分存两个原子字段，但一致性完全由同一 table 锁
   证明；字段使用 Relaxed，不虚构两个 Release store 的原子配对语义。

## 4. Docker 构建与 QEMU 结果

| 门禁 | 配置 | 结果 |
|------|------|------|
| RV64 kernel build | `CORE_NUM=8` | PASS，exit 0，约 123s |
| LA64 kernel build | `CORE_NUM=8` | PASS，exit 0，约 132s |
| RV64 futex LTP | `CORE_NUM=8` | PASS，301.398s，20 PASS / 6 SKIP / 0 FAIL |
| LA64 futex LTP | `CORE_NUM=8` | PASS，305.393s，20 PASS / 6 SKIP / 0 FAIL |

两架构均打印 `online_mask=0xff`，无 panic、fatal、timeout 或 runner forbidden marker。
每架构的 26 次由 musl/glibc 各 13 次组成；每套 libc 的 `futex_waitv01/02/03` 因用例要求
Linux 5.16、当前 uname 为 5.10 而 SKIP，其余 10 项 PASS。

有效 DeepSeek 任务：

- `smp-b65-shared-futex-key-design`
- `smp-b65-build-review-r2`
- `smp-b65-final-validation-r2`
- focused children: `agent-bda7ab481052-r01-rv64-futex-ltp`、
  `agent-bda7ab481052-r02-la64-futex-ltp`

GPT/Codex 独立解析原始 QEMU 日志复核了 PASS/SKIP/FAIL 数量；最终判定不直接依赖模型摘要。

## 5. 明确未验收

- **NOT RUN:** 专门制造旧 shared queue 存活、原物理页解除映射并把同一 PPN 反复分配给
  无关新页的动态竞态。当前由 backing pin 的静态生命周期证明排除错误命中。
- **NOT RUN:** `force_swap_out()` 或 truncate 强制替换 backing 后的 waiter 语义。该路径可
  绕过普通 strong-count 回收门槛，换入新 Arc 后可能形成 false-negative。
- **NOT RUN:** pin 对普通 swap/zram/page-cache 回收和内存压力的量化影响。
- **NOT RUN:** futex table 自旋锁内 faultable 用户读取；计划由 B66 改为锁外 fault-in +
  锁内 nofault 复查。
- 初赛 `mask=0x003` 未重复执行；B65 使用变更面更直接的双架构 futex LTP 门禁，不能把
  B64 的历史初赛账本记成 B65 新证据。
