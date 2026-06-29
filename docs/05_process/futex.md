---
title: "futex 与线程退出协作"
category: process
status: stable
author: MangoCore Team
last_update: 2026-06-29
tags: [process, futex, waitqueue]
---

# futex 与线程退出协作

## 1. 源码位置

| 文件 | 内容 |
|------|------|
| `os/src/syscall/process/futex.rs` | futex syscall 参数解析、key 选择、waitv |
| `os/src/task/threads.rs` | futex table、wait/wake/requeue 实现 |
| `os/src/task/manager.rs` | WaitQueue 和等待模板 |
| `os/src/task/task.rs` | `clear_child_tid` 退出唤醒 |

## 2. 支持的命令

`FutexCmd` 当前处理：

| 命令 | syscall 分支 |
|------|--------------|
| `FUTEX_WAIT` | 支持 |
| `FUTEX_WAKE` | 支持 |
| `FUTEX_REQUEUE` | 支持 |
| `FUTEX_CMP_REQUEUE` | 支持 |
| `FUTEX_WAIT_BITSET` | 支持 |
| `FUTEX_WAKE_BITSET` | 支持 |
| `FUTEX_WAITV` | 单独 syscall 支持 |
| PI futex / wake op / fd | 返回 `EINVAL` 或未支持 |

`FUTEX_PRIVATE_FLAG` 和 `FUTEX_CLOCK_REALTIME` 作为 option 解析。

## 3. futex key

futex key 分两类：

```rust
enum FutexKey {
    Private(usize),
    Shared(usize),
}
```

| key | 来源 |
|-----|------|
| private | 用户虚拟地址 |
| shared | 物理地址加页内偏移 |

非 private futex 并不总是 shared key。`futex_key_for()` 会询问当前 VM：

```rust
vm.futex_uses_shared_key(VirtAddr::from(uaddr))?
```

只有 VMA 真实为 shared mapping 时，才翻译到物理地址 key；否则仍使用 private key。

源码中 key 选择包含一次 VMA 语义判断和一次页表翻译：

```rust
fn va_to_phys_key(
    vm: &crate::mm::AddressSpace<crate::mm::KernelPageTableImpl>,
    va: usize,
) -> Option<usize> {
    let va = VirtAddr::from(va);
    let vpn = va.floor();
    let offset = va.page_offset();
    vm.translate(vpn).map(|ppn| (ppn.0 << 12) + offset)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FutexKey {
    Private(usize),
    Shared(usize),
}

fn futex_key_for(
    task: &TaskControlBlock,
    uaddr: usize,
    is_private: bool,
) -> Result<FutexKey, isize> {
    if is_private {
        return Ok(FutexKey::Private(uaddr));
    }

    let vm_ref = task.process.vm();
    let vm = vm_ref.lock();
    if vm.futex_uses_shared_key(VirtAddr::from(uaddr))? {
        va_to_phys_key(&vm, uaddr)
            .map(FutexKey::Shared)
            .ok_or(EFAULT)
    } else {
        Ok(FutexKey::Private(uaddr))
    }
}
```

key 的选择决定不同进程能否在同一个 futex 上相遇。private futex 使用用户虚拟地址作为 key，因此只在同一进程地址空间内有意义；shared futex 使用物理地址加页内偏移作为 key，使两个进程通过 `MAP_SHARED` 映射同一页时能够命中同一个等待队列。单纯清除 `FUTEX_PRIVATE_FLAG` 不足以生成 shared key，内核还要确认 VMA 本身确实是 shared mapping。

## 4. private 与 shared 表

| 类型 | 表 |
|------|----|
| private | `ProcessControlBlock::futex(): Arc<Mutex<Futex>>` |
| shared | 全局 `PROCESS_SHARED_FUTEX: BTreeMap<usize, WaitQueue>` |

`PROCESS_SHARED_FUTEX_MAYBE_NONEMPTY` 用作快速判断，调度循环降频调用 `compact_shared_futex()` 清理空 WaitQueue key。

表结构保存在 `task/threads.rs`：

```rust
pub struct Futex {
    inner: BTreeMap<usize, WaitQueue>,
}

#[derive(Clone, Copy)]
pub struct FutexWaitEntry {
    pub futex_word: UserPtr<u32>,
    pub futex_key: usize,
    pub val: u32,
}

fn wait_queue_for_key(map: &mut BTreeMap<usize, WaitQueue>, key: usize) -> &mut WaitQueue {
    map.entry(key).or_insert_with(WaitQueue::new)
}

fn wake_waiters(map: &mut BTreeMap<usize, WaitQueue>, key: usize, val: u32) -> isize {
    if let Some(mut wait_queue) = map.remove(&key) {
        let ret = wait_queue.wake_at_most(val as usize);
        if !wait_queue.is_empty() {
            map.insert(key, wait_queue);
        }
        ret as isize
    } else {
        0
    }
}
```

## 5. FUTEX_WAIT

`sys_futex()` 对 `FUTEX_WAIT`：

1. `uaddr` 非空且 4 字节对齐。
2. 读取 timeout，可为 null。
3. 非 private 路径先读取 futex word，确保地址可读。
4. 计算 private/shared key。
5. 调用 `do_futex_wait()` 或 `do_futex_wait_shared()`。

wait 逻辑：

| 用户 word | 返回 |
|-----------|------|
| 等于 val | 入队睡眠 |
| 不等于 val | `EAGAIN` |
| 读取失败 | 对应 errno |

相对 timeout 会转换为 monotonic deadline。

`sys_futex()` 的分发主干如下，所有 wait/wake/requeue 都先完成用户地址和 option 校验：

```rust
pub fn sys_futex(
    uaddr: *mut u32,
    futex_op: u32,
    val: u32,
    timeout: *const TimeSpec,
    uaddr2: *mut u32,
    val3: u32,
) -> isize {
    let token = current_user_token();
    if uaddr.is_null() || uaddr.align_offset(4) != 0 {
        return EINVAL;
    }
    let futex_word = UserPtr::new(uaddr as *const u32);
    let cmd = threads::FutexCmd::from_primitive(futex_op & 0x7fu32);
    let option = FutexOption::from_bits_truncate(futex_op);
    let is_private = option.contains(FutexOption::PRIVATE);
    let private_key = uaddr as usize;
    match cmd {
        FutexCmd::Wait => {
            let timeout = match read_timeout(timeout, token) {
                Ok(timeout) => timeout,
                Err(errno) => return errno,
            };
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Shared(phys_key)) => {
                    do_futex_wait_shared(futex_word, token, val, timeout, phys_key)
                }
                Ok(FutexKey::Private(key)) => do_futex_wait(futex_word, token, key, val, timeout),
                Err(errno) => errno,
            }
        }
        FutexCmd::WaitBitset => {
            if val3 == 0 {
                return EINVAL;
            }
            let deadline = match futex_bitset_deadline(timeout, token, option) {
                Ok(deadline) => deadline,
                Err(errno) => return errno,
            };
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Shared(phys_key)) => {
                    do_futex_wait_bitset_shared(futex_word, token, val, deadline, phys_key)
                }
                Ok(FutexKey::Private(key)) => {
                    do_futex_wait_bitset(futex_word, token, key, val, deadline)
                }
                Err(errno) => errno,
            }
        }
        FutexCmd::Wake | FutexCmd::WakeBitset => {
            if val > i32::MAX as u32 {
                return EINVAL;
            }
            if cmd == FutexCmd::WakeBitset && val3 == 0 {
                return EINVAL;
            }
            if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            match current_futex_key(private_key, is_private) {
                Ok(FutexKey::Private(key)) => {
                    current_task_ref().unwrap().process.futex().lock().wake(key, val)
                }
                Ok(FutexKey::Shared(phys_key)) => futex_wake_shared(phys_key, val),
                Err(errno) => errno,
            }
        }
        FutexCmd::Requeue | FutexCmd::CmpRequeue => {
            if uaddr2.is_null() || uaddr2.align_offset(4) != 0 {
                return EINVAL;
            }
            match UserPtr::new(uaddr2 as *const u32).read(token) {
                Ok(_) => {}
                Err(errno) => return errno,
            };
            if cmd == FutexCmd::CmpRequeue {
                match futex_word.read(token) {
                    Ok(value) if value == val3 => {}
                    Ok(_) => return EAGAIN,
                    Err(errno) => return errno,
                }
            } else if !is_private {
                if let Err(errno) = futex_word.read(token) {
                    return errno;
                }
            }
            let val2 = timeout as usize;
            if val > i32::MAX as u32 || val2 > i32::MAX as usize {
                return EINVAL;
            }
            let key = match current_futex_key(private_key, is_private) {
                Ok(key) => key,
                Err(errno) => return errno,
            };
            let key2 = match current_futex_key(uaddr2 as usize, is_private) {
                Ok(key) => key,
                Err(errno) => return errno,
            };
            match (key, key2) {
                (FutexKey::Private(key), FutexKey::Private(key2)) => {
                    current_task_ref()
                        .unwrap()
                        .process
                        .futex()
                        .lock()
                        .requeue(key, key2, val, val2)
                }
                (FutexKey::Shared(key), FutexKey::Shared(key2)) => {
                    futex_requeue_shared(key, key2, val, val2)
                }
                _ => EINVAL,
            }
        }
        FutexCmd::Invalid => EINVAL,
        _ => EINVAL,
    }
}
```

## 6. FUTEX_WAIT_BITSET

差异：

| 条件 | 行为 |
|------|------|
| `val3 == 0` | `EINVAL` |
| timeout | 按绝对 deadline 处理 |
| `FUTEX_CLOCK_REALTIME` | realtime deadline 转 monotonic deadline |

当前 wake bitset 分支只校验 `val3 != 0`，唤醒仍按 key 和数量处理。

## 7. FUTEX_WAKE

wake 分支：

| 条件 | 错误 |
|------|------|
| val > `i32::MAX` | `EINVAL` |
| `FUTEX_WAKE_BITSET` 且 val3 == 0 | `EINVAL` |

private wake 调用当前进程 futex table；shared wake 调用全局 shared table。

返回唤醒数量。

## 8. requeue/cmp_requeue

`FUTEX_REQUEUE` 和 `FUTEX_CMP_REQUEUE`：

1. `uaddr2` 非空且 4 字节对齐。
2. 读取 `uaddr2` 验证可读。
3. `CMP_REQUEUE` 需要 `*uaddr == val3`，否则 `EAGAIN`。
4. `val` 和 `val2` 不能超过 `i32::MAX`。
5. key1/key2 必须同为 private 或同为 shared。
6. 先 wake 最多 `val` 个，再把最多 `val2` 个 waiter 移到 key2。

private/shared 混合返回 `EINVAL`。

## 9. waitv

`sys_futex_waitv()` 支持 futex2 waitv 子集：

| 限制 | 错误 |
|------|------|
| flags 非 0 | `EINVAL` |
| nr_futexes == 0 或 > 128 | `EINVAL` |
| waiters null | `EFAULT` |
| waiter reserved 非 0 | `EINVAL` |
| flags 含不支持位 | `EINVAL` |
| futex size 不是 32-bit | `EINVAL` |
| val > u32::MAX | `EINVAL` |
| uaddr 为 0 | `EFAULT` |
| uaddr 未 4 字节对齐 | `EINVAL` |
| waiters 混用 private/shared option | `EINVAL` |
| 实际 key 表混用 private/shared | `EINVAL` |

返回值为被唤醒的 waiter index；timeout 返回 `ETIMEDOUT`，信号返回 `EINTR`。

## 10. 精确 timeout 优化

`threads.rs` 对短 timeout 有优化：

| 情况 | 行为 |
|------|------|
| 单线程进程 |
| ready queue 无其他任务 |
| timeout <= 150ms |

满足时 `try_single_thread_short_timeout()` 通过短自旋精确等待，避免调度/定时器尾部误差。

对于一般等待，`futex_wait_block_deadline()` 会保留尾部 spin guard：

| 架构 | guard |
|------|-------|
| loongarch64 | 12 ms |
| 其他 | 1.25 ms |

la64 相对 timeout 还带 180 us exit bias。

## 11. 等待模板

futex 使用 `futex_wait_event_interruptible_timeout_locked()`，它在持 futex table 锁下：

1. 检查条件。
2. 加入 WaitQueue。
3. 再检查条件、timeout、signal。
4. 挂 timeout timer。
5. 调用 `block_current_and_run_next_with_lock_checked()`。
6. 醒来后 `finish_wait()`。
7. 如果 waiter 已被 wake 移除，返回正常 wake 结果。

这保证 futex value 检查与入队之间不会丢失 wake。

等待模板的核心实现如下：

```rust
fn futex_wait_event_interruptible_timeout_locked<T, Q, F>(
    lock: &spin::Mutex<T>,
    mut queue_of: Q,
    mut cond: F,
    deadline: Option<TimeSpec>,
    normal_wake_result: isize,
) -> WaitResult
where
    Q: for<'a> FnMut(&'a mut T) -> &'a mut WaitQueue,
    F: FnMut(&mut T) -> Option<isize>,
{
    {
        let mut guard = lock.lock();
        if let Some(res) = cond(&mut guard) {
            return WaitResult::Ready(res);
        }
    }

    loop {
        let mut guard = lock.lock();
        if deadline_expired(deadline) {
            return WaitResult::TimedOut;
        }

        let task = current_task().unwrap();
        queue_of(&mut guard).prepare_to_wait(Arc::downgrade(&task));

        if let Some(res) = cond(&mut guard) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::Ready(res);
        }
        if deadline_expired(deadline) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::TimedOut;
        }
        if has_actionable_signal(&task) {
            queue_of(&mut guard).finish_wait(task.as_ref());
            return WaitResult::Interrupted;
        }
        discard_non_actionable_unblocked_signals(&task);

        let block_deadline = futex_wait_block_deadline(deadline);
        if let Some(real_deadline) = deadline {
            if block_deadline == Some(real_deadline) {
                drop(guard);
                return futex_wait_tail_spin(
                    lock,
                    queue_of,
                    &mut cond,
                    &task,
                    real_deadline,
                    normal_wake_result,
                );
            }
        }
        if let Some(block_deadline) = block_deadline {
            wait_with_timeout(Arc::downgrade(&task), block_deadline);
        }
        drop(task);

        block_current_and_run_next_with_lock_checked(guard, |task| {
            let no_signal = !has_actionable_signal(task);
            let not_timed_out = block_deadline
                .map(|deadline| TimeSpec::now() < deadline)
                .unwrap_or(true);
            no_signal && not_timed_out
        });

        let task = current_task_ref().unwrap();
        let mut guard = lock.lock();
        let removed = queue_of(&mut guard).finish_wait(task);
        drop(guard);
        task.acquire_inner_lock().refresh_real_timer();

        if !removed {
            return WaitResult::Ready(normal_wake_result);
        }
    }
}
```

唤醒方会把 waiter 从队列中移除；等待方醒来后如果 `finish_wait()` 发现自己已经不在队列中，就返回 `normal_wake_result`。这就是 futex wake 不需要额外传递事件对象的原因。

## 12. clear_child_tid

线程退出时，TCB 会：

1. 向 `clear_child_tid` 用户地址写 0。
2. 唤醒 private futex key `clear_child_tid`。
3. 如果该地址所在 VMA 使用 shared key，也唤醒物理 key。
4. 若 fault 前后物理 key 变化，同时唤醒旧 key 和新 key。

该行为用于 pthread join 类路径。

## 13. 错误转换

futex wait 内部 `WaitResult` 转换：

| WaitResult | futex errno |
|------------|-------------|
| `Ready(value)` | value |
| `Interrupted` | `EINTR` |
| `TimedOut` | `ETIMEDOUT` |

这与普通 WaitQueue 的 `-ERESTART/-EAGAIN` 默认转换不同。

## 14. 调试核对点

| 现象 | 检查 |
|------|------|
| process-shared futex 不能跨进程唤醒 | VMA 是否真正 shared，phys key 是否一致 |
| futex_wait 立即 EAGAIN | 用户 word 是否等于 val |
| waitv 返回 EINVAL | waiter flags、32-bit size、private/shared 混用 |
| timeout 偏差大 | short timeout spin guard 与 arch bias |
| pthread join 卡住 | clear_child_tid 写 0 和 futex wake |
