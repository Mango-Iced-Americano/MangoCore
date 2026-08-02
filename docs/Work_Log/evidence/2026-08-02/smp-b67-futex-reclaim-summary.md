---
title: "B67 shared futex backing 与 OOM 回收证据"
date: 2026-08-02
status: partial
phase: B67
---

# B67 shared futex backing 与 OOM 回收证据

## 1. 验收范围

B67 只收口匿名页回收与 B65 shared-futex resident backing key 的生命周期冲突：deep clean
不得绕过非空 futex queue 持有的 backing `Arc`，临时 pin 导致的 `SharedPage` 也不能永久
丢出回收候选。文件 truncate/page-cache invalidate 属于 FS backing 生命周期，留给后续协作。

阶段状态为 `partial`：生产路径、所有权证明、双架构 8 核编译和判别性 MM ktest 已通过；
真实 OOM 与多 waiter 并发、zram/swap 耗尽组合、文件 truncate 竞态仍为 NOT RUN。

## 2. 根因与实现

旧 `Frame::force_swap_out()` 会在不检查引用计数的情况下把单个 VMA entry 改为
`SwappedOut`。futex queue、SysV SHM registry 或其它 VMA 仍持有旧 `Arc<FrameTracker>`；
换入又会创建新 frame。结果可能是 WAKE 解析 `EFAULT`，或以新 Arc identity 查询不到旧队列，
SysV SHM 也可能分裂成不同物理页。

B67 做了三项收口：

1. 删除 `force_swap_out()` 和 `Vma::force_swap()`；deep clean 仍扩大匿名 VMA 扫描范围，
   但单页统一走尊重 `Arc::strong_count` 的 `do_oom()`。
2. `do_oom()` 每轮只扫描入口时的 active 候选数。遇到 `SharedPage` 时把 VPN 放回队尾，
   避免同一轮死循环，也使 waiter 离开、pin 解除后的下一轮仍能回收。
3. 新增 `mm::shared_futex_pin_blocks_reclaim`：低地址匿名 `MAP_SHARED` 页被 pin 时保持 PTE
   与 Arc identity；解除 pin 后第二轮 deep clean 必须压缩并清除 PTE，随后 fault-in 恢复。

Linux 的共享 futex key 使用 inode sequence、mapping page offset 和页内偏移，能够独立于
resident physical frame。MangoCore 尚无统一 shmem/anon-vma backing object，因此 B67 采用
更保守的 owner pin；依据见 Linux 官方
`https://www.kernel.org/doc/html/latest/kernel-hacking/locking.html` 的 futex API 说明。

## 3. 环境与源码

- Worktree: `/home/lzm/projects/MangoCore-smp-integration-20260725`
- Branch/基线: `smp` / `3c196af0851ee9744e6275984679cd3ae2e81fc4`
- Docker container: `mangocore-smp-integration-20260725-os-dev-1`
- Image: `zhouzhouyi/os-contest:20260510`
- Image ID: `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest: `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- QEMU: RV64 10.0.2 / LA64 10.0.2
- 四个生产/测试文件的 binary diff SHA-256:
  `3836c702d405eab61246269b0f3239d23e712cc3286305544884987eb7bb1343`

## 4. DeepSeek 协作与 Docker 结果

DeepSeek `smp-b67-force-swap-review-r1` 只读审查同意删除 force 路径，不建议 B67 跨入 FS
构建完整逻辑 backing ID。GPT/Codex 额外识别并修复了 `SharedPage` 被永久移出 active 队列的
问题，未照搬模型遗漏该生命周期的最小建议。

| 门禁 | Child/job | 配置 | 结果 |
|------|-----------|------|------|
| RV64 kernel build | `agent-aa93196b2315-r01-rv64-kernel-build` | `CORE_NUM=8` | PASS，exit 0，128.416s，无 mutation |
| LA64 kernel build | `agent-04f8c063ce9d-r02-la64-kernel-build` | `CORE_NUM=8` | PASS，exit 0，138.038s，无 mutation |
| RV64 MM ktest | `agent-04f8c063ce9d-r03-rv64-ktest` | `CORE_NUM=8 KTEST=mm` | PASS，5/5，`online_mask=0xff` |
| LA64 MM ktest | `agent-04f8c063ce9d-r04-la64-ktest` | `CORE_NUM=8 KTEST=mm` | PASS，5/5，`online_mask=0xff` |

双架构新用例均打印 `ok 5 mm::shared_futex_pin_blocks_reclaim`，无 panic、fatal、timeout 或
forbidden marker。两个 ktest 和 LA64 build 的 source before/after tracked diff 均为
`32c25cc52b8cbb9c394b24b0f756ef916fb106171888d4b09ace9f1a05d5c1dc`；RV64 补验也有一致
指纹，wrapper 最终状态为 `SUCCEEDED/REVIEWED`。

首次总任务 `smp-b67-validation-r1` 在 RV64 编译期间发生 Codex 文档写入；该子进程虽然 exit 0，
但 `mutation_detected=true`，父任务正确标记 `FAILED`。这条记录不计 PASS，也不采用 DeepSeek
原始汇总中“全部 source 指纹一致”的错误说法；稳定补验是上表第一项。

## 5. 未验收边界

- **NOT RUN:** 多 CPU 的真实 futex WAIT/WAKE/requeue 与 OOM victim 回收精确交错。
- **NOT RUN:** zram 满、swap 可用/耗尽和多个 `SharedPage` 候选组合。
- **NOT RUN:** 文件 truncate/page-cache invalidate 替换 file-backed shared futex backing。
- **NOT RUN:** 长时间大量 waiter pin 的内存压力与回收吞吐量化。
- 初赛 `mask=0x003` 和 futex LTP 未机械重跑；B67 由直接区分旧 force/active 行为的双架构
  MM ktest 覆盖，B66 已有同一基线上的双架构 futex LTP 20 PASS + 6 版本 SKIP。
