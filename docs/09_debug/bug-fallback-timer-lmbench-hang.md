---
title: "Bug: I/O Fallback Timer 导致 lmbench context switch 挂死"
category: debug
status: verified
author: MangoCore Team
last_update: 2026-06-19
tags: [timer, fallback, lmbench, context-switch, race]
---

# Bug: I/O Fallback Timer 导致 lmbench context switch 挂死

## 症状

lmbench-musl 跑到 `context switch overhead` 测试后系统挂死。不 crash、不 panic，trace 触发仍响应但显示 0 条调度记录。同一 Docker 镜像在另一台机器正常。

```
context switch overhead

"size=32k ovr=214.67
2 100.46
4 128.90
... (卡住)
[trace] dump start (0 entries) [trigger: schedule]
```

## 根因

两处 bug 叠加：

### Bug 1: `wait_with_timeout()` 生成 `fallback_ms: None`，stale timer 无法 re-arm

`wait_event_impl` 的 I/O fallback 路径调用 `wait_with_timeout()` arm timer，但该函数始终创建 `TimerAction::WakeTask { fallback_ms: None }`。`run_timer()` 中 stale fallback re-arm 逻辑只在 `fallback_ms: Some(ms)` 时生效。旧 timer 到期后被静默丢弃 → 任务永久阻塞。

**修复**: I/O fallback 路径直接调用 `add_kernel_timer(TimerAction::WakeTask { fallback_ms: Some(ms), ... })`。

### Bug 2: timer 在任务变成 `Interruptible` 之前触发，被消费者丢弃

时间窗口：
```
wait_event_impl:
  1. cond() 检查 → 无数据
  2. arm fallback timer ← timer 在此处挂入
  3. drop(task)
  4. block_current_and_run_next_with_lock_checked():
     a. take_current_task()
     b. acquire_inner_lock() → task_status = Interruptible ← 状态在此处切换
     c. schedule()
```

如果在步骤 2 和 4b 之间 timer 触发，`run_timer()` 看到 `task_status != Interruptible`，timer 被消费但不唤醒任务。`wait_io_timer_pending` 已被清空，任务重新进入等待循环时会再次 arming，但若反复命中此窗口则形成 busy-wait-sleep 循环，大量消耗 CPU 导致 pipe 对端写者饥饿。

**修复**: `run_timer()` 中，当 fallback timer 触发但 `task_status != Interruptible` 时，re-arm timer 而不是消费它。

### Bug 3: `WAIT_IO_FALLBACK_MS = 1ms` 加剧问题

1ms fallback 在单核慢机器上触发过于频繁，timer 唤醒 → 检查 pipe（无数据）→ 重新阻塞的循环消耗大量 CPU，pipe 写者得不到调度。

**修复**: 恢复为 `WAIT_IO_FALLBACK_MS = 10ms`。

## 修复内容

**文件**: `os/src/task/manager.rs`

1. fallback 路径改用 `add_kernel_timer()` 直接创建 `fallback_ms: Some(ms)` 的 timer
2. `run_timer()` 中对非 `Interruptible` 状态的 fallback timer 执行 re-arm
3. `WAIT_IO_FALLBACK_MS` 保持 10ms

```rust
// Bug 1 fix: arm with fallback_ms: Some(ms)
let generation = task.wait_timer_generation.fetch_add(1, Relaxed).wrapping_add(1);
add_kernel_timer(
    TimerAction::WakeTask { task: Arc::downgrade(&task), generation, fallback_ms: Some(ms) },
    TimeSpec::now() + TimeSpec::from_ms(ms),
);

// Bug 2 fix: re-arm if not Interruptible yet
let inner = task.acquire_inner_lock();
if inner.task_status != super::TaskStatus::Interruptible {
    drop(inner);
    // re-arm instead of consuming
    ...
    return false;
}
drop(inner);
// fall through to normal wake
```

## 验证

- `make rv64-kernel-build-only` ✅
- QEMU lmbench-musl 完整通过 ✅

## 教训

1. **timer 与任务状态切换的时序窗口**：任何在任务进入阻塞状态前 arm 的 timer 都有被过早触发并丢弃的风险。必须在 timer callback 中检查任务状态，必要时 re-arm。
2. **`wait_with_timeout()` 是 deadline timer，不是 fallback timer**：不要复用 — deadline timer 一次性消费即可，fallback timer 必须有 stale re-arm 能力。
3. **机器相关 = 时序竞态**：同 Docker 不同机器的表现差异，几乎可以确定是 timing race。
4. **GDB 排障提示**：release build 变量大量被优化，但 `ptype` 可看 struct 布局，`x/gx` 可 dump 内存。`lazy_static!` 公开符号不一定是真实数据入口，应打断点在方法上看 `self`。
