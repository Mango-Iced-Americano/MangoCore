---
title: "Timer/Timekeeping 修复对照实验报告"
category: debug
status: verified
author: MangoCore Team
last_update: 2026-06-18
tags: [timer, timekeeping, timerfd, posix-timer, clock-settime, qemu]
---

# Timer/Timekeeping 修复对照实验报告

## 目标

验证 2026-06-18 前后 6 个 timer/timekeeping 修复提交相对原版是否产生可观测差异，重点确认：

1. `CLOCK_REALTIME` 相对 timer 不应被 `clock_settime()` 的 wall-clock 跳变提前触发。
2. `CLOCK_REALTIME` 绝对 timerfd/POSIX timer 在 wall-clock 跳变后应重新定位 deadline。
3. `clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME)` 在 wall-clock 跳过目标时间后应及时返回。
4. 修复不能破坏双架构 kernel build、timer smoke 和 basic smoke。

本轮按用户要求不跑 LTP；LTP 不作为 push 前置条件。

## 对照对象

| 角色 | commit | 说明 |
|---|---|---|
| 原版基线 | `1096f4d2` | 当前 6 个 timer 修复提交之前的父提交 |
| 候选版本 | `e894ee1e` | `Fix realtime timer clock jumps`，包含 6 个未 push timer 相关提交 |

候选提交序列：

```text
e894ee1e Fix realtime timer clock jumps
5f1d5f34 Wire timerfd into high-res queue
20cd3697 Improve LTP timer focused harness
adc90938 Fix timer deadline rounding and la64 TLB invalidation
8b376703 feat: one-shot high-res timer + fix clock_getres (Phase 2 & 3)
956ccf22 fix: safe time conversion + irq-safe KERNEL_TIMER_QUEUE (Phase 1)
```

## 控制变量

为保证原版/候选对比只体现内核 timer 修复差异，实验控制如下：

| 控制项 | 做法 |
|---|---|
| 测试 probe | 原版只临时移植 `user/src/bin/initproc.rs`、`user/src/syscall.rs` 的 timer smoke probe，不移植任何内核修复 |
| 内核产物 | 原版和候选都重新执行对应架构 `*-kernel-build-only`，避免复用旧 `kernel-rv`/`kernel-la` |
| 上层应用 | 对原版显式把重新构建的 `/initproc` 写入 sdcard，避免测试盘旧 `/sdcard/initproc` 或候选 `initproc` 污染结果 |
| QEMU 配置 | 两边使用同一 `make rv64-run` / `make la64-run` 路径，同一 `LOG=error` |
| 测试配置 | `mask=0x000` 且 `timer_smoke=1`；basic smoke 使用 `mask=0x001` 且 `timer_smoke=0` |
| 镜像状态 | 测试后恢复 `/os_test.conf` 为仓库默认 `os_test.conf` |
| 架构顺序 | rv64/la64 顺序执行，不并行构建，避免共享架构生成状态互相覆盖 |

## Timer Smoke 用例

`timer_smoke=1` 覆盖以下路径：

| 用例 | 目的 | 通过阈值 |
|---|---|---|
| `timerfd_create(CLOCK_MONOTONIC)` 2ms one-shot | 验证 timerfd 已接入 high-res timer queue | `elapsed_ms <= 50` |
| `timerfd_create(CLOCK_REALTIME)` 相对 80ms + `clock_settime(+2s)` | 验证 realtime 相对 timer 不受 wall-clock 跳变影响 | `50ms <= elapsed_ms <= 500ms` |
| `timerfd CLOCK_REALTIME | TFD_TIMER_ABSTIME` 周期 timer + `clock_settime(+2s)` | 验证首次到期后的绝对周期 timer 仍保留 wall-clock 目标并重定位 | 第二次 read 快速返回，`elapsed_ms <= 120` |
| POSIX `timer_create(CLOCK_REALTIME, SIGEV_NONE)` absolute + `clock_settime(+2s)` | 验证 POSIX realtime absolute timer 被 clock jump 重定位 | `remaining_ms == 0` |
| `clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME)` + `clock_settime(+2s)` | 验证 realtime absolute sleep 被 clock jump 唤醒后重判定 | child `elapsed_ms <= 500` |

## 候选版本验证结果

候选版本 `e894ee1e` 的完整验证链：

| 项目 | rv64 | la64 |
|---|---|---|
| `make rv64-kernel-build-only` / `make la64-kernel-build-only` | 通过 | 通过 |
| timer smoke QEMU | 通过 | 通过 |
| basic smoke QEMU | 通过 | 通过 |
| `git diff --check` | 通过 | 通过 |
| `lang_items.rs` 残留差异 | 无 | 无 |

候选 timer smoke 实测：

| 用例 | rv64 | la64 |
|---|---:|---:|
| `timerfd CLOCK_MONOTONIC 2ms` | 4ms PASS | 3ms PASS |
| `CLOCK_REALTIME` 相对 80ms + `clock_settime(+2s)` | 81ms PASS | 80ms PASS |
| realtime absolute periodic rearm | `first=1 second=10 elapsed=1ms` PASS | `first=1 second=10 elapsed=1ms` PASS |
| POSIX realtime absolute timer | `remaining_ms=0` PASS | `remaining_ms=0` PASS |
| `clock_nanosleep` realtime absolute | 20ms PASS | 22ms PASS |

候选 basic smoke：

| 架构 | musl | glibc |
|---|---|---|
| rv64 | `exit_code=0`, 4s | `exit_code=0`, 4s |
| la64 | `exit_code=0`, 11s | `exit_code=0`, 12s |

la64 basic 过程中仍有已知 shell `getcwd` warning，但 basic 脚本最终 `exit_code=0`，不属于本轮 timer 修复回归失败。

## 原版基线对照结果

### 无效样本剔除

第一次基线尝试直接在 `1096f4d2 + probe` 上运行 `make rv64-run`，日志看似通过，但该样本被剔除。原因：

- `make rv64-run` 的 `comp` 目标直接使用已有 `../kernel-rv`。
- 当时未先重建原版 `kernel-rv`，可能复用了候选版本内核产物。

第二次基线尝试先重建原版 kernel，但日志中配置行仍没有 `timer_smoke=true` 字段，也被剔除。原因：

- `kernel-build-only` 会重建 user/initramfs/kernel，但不会把新的 `initproc` 写入 sdcard。
- QEMU stage-1 会执行测试盘上的 `/sdcard/initproc` 或镜像内旧 `/initproc`，导致用户态 probe 未实际运行。

最终有效基线做法：

1. `git switch --detach 1096f4d2`
2. 只应用用户态 probe patch 到 `user/src/bin/initproc.rs`、`user/src/syscall.rs`
3. `make rv64-kernel-build-only`
4. 注入 `timer_smoke=1` 到 `sdcard-rv.img`
5. 用 `debugfs` 把 `user/target/riscv64gc-unknown-none-elf/release/initproc` 写入 `sdcard-rv.img:/initproc`
6. `make rv64-run`

### rv64 有效对照

| 用例 | 原版 `1096f4d2 + probe` | 候选 `e894ee1e` |
|---|---:|---:|
| `timerfd CLOCK_MONOTONIC 2ms` | 4ms PASS | 4ms PASS |
| `CLOCK_REALTIME` 相对 80ms + `clock_settime(+2s)` | **1ms FAIL** | **81ms PASS** |

原版有效失败日志摘要：

```text
[timer_smoke] timerfd monotonic one-shot begin
[timer_smoke] read expirations=1 elapsed_ms=4
[timer_smoke] PASS
[timer_smoke] realtime relative settime isolation begin
[timer_smoke] realtime relative expirations=1 elapsed_ms=1
[timer_smoke] realtime relative result out of range
[initproc] timer_smoke failed
```

结论：原版 `CLOCK_REALTIME` 相对 timerfd 在 arm 后执行 `clock_settime(+2s)`，80ms 相对等待被错误缩短为 1ms。候选版本同一 probe 为 81ms，通过阈值。该对照直接证明修复消除了“相对 realtime timer 被 wall-clock 跳变影响”的底层语义错误。

### la64 基线记录

la64 原版基线也执行了相同准备流程：

1. 重新构建原版 la64 kernel。
2. 注入 `timer_smoke=1` 到 `sdcard-la.img`。
3. 写入原版+probe 的 `loongarch64-unknown-linux-gnu/release/initproc` 到 `sdcard-la.img:/initproc`。
4. 运行 `make la64-run`。

结果：内核启动到 stage-1 后超过 90 秒无新增输出，手动终止 QEMU：

```text
qemu-system-loongarch64: terminating on signal 15
```

该样本说明原版 la64 在同一 harness 下无法完成 timer smoke，但没有进入 timer assertion，因此不纳入定量 timer 对比。定量证明以 rv64 有效失败样本为准。

## 结论

1. 已完成严格意义上的“原版 vs 候选”对照实验，至少在 rv64 上形成明确失败/通过差异。
2. 原版失败点与本轮核心修复目标一致：`CLOCK_REALTIME` 相对 timer 错误绑定 wall-clock，`clock_settime(+2s)` 后 80ms 等待在 1ms 内提前到期。
3. 候选版本在 rv64/la64 均通过 timer smoke 与 basic smoke，说明修复覆盖底层 timer 语义和相关上层 initproc 应用路径，未观察到基础回归。
4. la64 原版基线 hang 只能作为兼容性/启动行为记录，不作为 timer 语义定量证据。
5. 当前分支 `develop...origin/develop [ahead 6, behind 21]`，不建议直接 push；应先同步远端 21 个提交，再重跑候选双架构 build、timer smoke、basic smoke 后 push。

## 后续建议

push 前最低验证清单：

```text
同步 origin/develop
make rv64-kernel-build-only
make la64-kernel-build-only
rv64 timer_smoke
la64 timer_smoke
rv64 basic smoke
la64 basic smoke
git diff --check
lang_items.rs 无残留差异
```

如需进一步提高统计可信度，可把 rv64 有效对照中的 timer smoke 重复 3 次，并保存每次 QEMU stdout 到 `testresult/`，但当前 1ms FAIL vs 81ms PASS 的差距已经足以证明修复效果。
