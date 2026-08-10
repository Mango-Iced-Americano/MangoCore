---
title: "OOM、overcommit 与 locked pages"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-11
tags: [mm, oom, overcommit, mlock]
---

# OOM、overcommit 与 locked pages

## 1. OOM 相关源码

MM 中与 OOM 和内存限制相关的实现分布在多个文件：

| 文件 | 职责 |
|------|------|
| `os/src/mm/heap_allocator.rs` | 内核堆分配失败 recovery、fatal handler、堆统计 |
| `os/src/mm/frame_allocator.rs` | 物理页分配失败 recovery、`frame_reserve()` |
| `os/src/mm/frame_store.rs` | 页面压缩/换出状态 |
| `os/src/mm/vma.rs` | VMA 级压缩/换出候选扫描 `do_oom()` |
| `os/src/mm/address_space.rs` | 地址空间级 shallow/deep clean、locked pages |
| `os/src/mm/sysctl.rs` | overcommit、max_map_count、min_free_kbytes 等 sysctl 状态 |
| `os/src/mm/mmap.rs` | mmap/brk overcommit 检查 |

OOM 行为受 feature 控制。未启用 `oom_handler` 时，部分回收路径为空或不可用；启用后才有 zram/swap 相关状态。

## 2. 实现状态

| 功能 | 状态 |
|------|------|
| 内核堆 OOM recovery | `KernelAllocator`（slab + buddy）最多重试 3 次，失败后 fatal handler 关机 |
| 物理页 OOM recovery | `frame_alloc()` 在 `oom_handler` feature 下调用回收后重试 |
| overcommit | `sysctl.rs` 提供 overcommit_memory/ratio 与 commit limit |
| locked pages | `AddressSpace` 维护 locked page 标记，`mlock`/`mlock2` 按 rlimit 校验 |
| 压缩/换出 | `oom_handler` feature 下通过 `Frame::zip()`/`swap_out()` 和 VMA clean 路径启用 |
| shared page 回收 | 所有 shallow/deep 路径都尊重 backing `Arc`；有外部共享或 pin 时延后回收 |

## 3. 内核堆 OOM

`KernelAllocator` 的分配逻辑：

```text
alloc(layout)
  ├── 最多尝试 3 次
  ├── slab_class_for(layout) → slab alloc 或 inner.heap.alloc(layout)
  ├── 成功: 记录 perf 与 heap gauge
  ├── 失败: recover_for(layout)
  └── recovery 失败: 返回 null
```

`recover_for()` 用 `OOM_RECOVERY_IN_PROGRESS` 防止递归 recovery。启用 `oom_handler` 时，它按 layout 大小估算需要的页数并调用 `frame_allocator::oom_handler(pages)`。

如果最终进入 `handle_alloc_error()`：

1. 打印 `HEAP ALLOCATION FAILED`。
2. 输出当前 syscall 名称。
3. 输出 layout 和 `KERNEL_HEAP_SIZE`。
4. 若启用 `heap_trace`，dump OOM backtrace。
5. 调用 `hal::shutdown()`。

该路径不调度任务退出，避免持锁栈帧无法析构。

堆 OOM 的处理原则是“allocator 返回 null，最终 fatal handler 关机”，不是在分配器内部杀当前进程。这样可以避免分配失败发生在持锁或不可安全调度的路径上。

### 3.1 Heap allocator 诊断口径

`perf_stats` 的 `memory_io` profile 通过 `/sys/kernel/stats/heap` 导出 schema v1。
正常 profile 关闭时记录函数为 no-op。计数器分为四组：

- 锁归因：alloc/dealloc 各自的 wait/hold 总 ticks，以及每 CPU calls/wait/hold；
- 分布：wait/hold 使用 `0`、`1..63`、`64..1023`、`1024..16383`、
  `16384..262143`、`>=262144` 六个原始 tick 桶，区分持续竞争与少数长尾；
- 路径：各 slab class、slab fast hit、新页 refill、slab fallback 和 direct buddy；
- 压力：请求字节、buddy failure、recovery attempt/success、retry 和最终失败。

分析时先看 `heap_lock_wait_ticks_total / heap lock calls` 与每 CPU 分布判断全局锁竞争，
再用 hold histogram 区分临界区本身过长还是排队过长。若 refill/fallback 比例高而 wait
不高，优先检查 slab page 周转；若 wait 高且各 CPU 均匀增长，则全局 allocator 锁是
SMP 扩展瓶颈候选。所有时间字段均为架构原始 tick，只能在同架构、同 QEMU 配置下 A/B。

## 4. 物理页 OOM

`frame_alloc()` 失败时，在 `oom_handler` 特性下会尝试回收用户页：

```text
frame_alloc()
  ├── StackFrameAllocator::alloc()
  ├── if none:
  │     ├── oom_handler(1)
  │     └── retry alloc()
  └── Option<Arc<FrameTracker>>
```

`frame_reserve(pages)` 是上层预留入口。`fault_in_user_va()` 和 `fault_in_trap_va()` 会先调用 `frame_reserve(3)`，为页表页、数据页和元数据留出空间。

`Frame` 自身提供压缩和换出接口，普通路径会拒绝 shared page：

```rust
#[cfg(feature = "oom_handler")]
pub fn swap_out(&mut self) -> Result<Arc<FrameTracker>, MemoryError> {
    match self {
        Frame::InMemory(frame_ref) => {
            if Arc::strong_count(frame_ref) == 1 {
                let tracker = SWAP_DEVICE.lock().write(frame_ref.ppn.get_bytes_array())?;
                let old = core::mem::replace(self, Frame::SwappedOut(tracker));
                let Frame::InMemory(frame) = old else { unreachable!() };
                Ok(frame)
            } else {
                Err(MemoryError::SharedPage)
            }
        }
        _ => Err(MemoryError::NotInMemory),
    }
}

#[cfg(feature = "oom_handler")]
pub fn zip(&mut self) -> Result<Arc<FrameTracker>, MemoryError> {
    match self {
        Frame::InMemory(frame_ref) => {
            if Arc::strong_count(frame_ref) == 1 {
                let tracker = ZRAM_DEVICE
                    .lock()
                    .write(frame_ref.ppn.get_bytes_array())
                    .map_err(|_| MemoryError::ZramIsFull)?;
                let old = core::mem::replace(self, Frame::Compressed(tracker));
                let Frame::InMemory(frame) = old else { unreachable!() };
                Ok(frame)
            } else {
                Err(MemoryError::SharedPage)
            }
        }
        _ => Err(MemoryError::NotInMemory),
    }
}
```

返回旧 `FrameTracker` 是为了让调用者先撤销 PTE、提交 TLB 失效，再在 `MmuGather` 退休队列中
释放物理页；它不是 swap/zram slot ID。

这就是 shared anonymous、COW 共享页和 PageCache 共享页不会被普通 `do_oom()` 直接压缩/换出的依据。

## 5. VmPageStore 状态

启用 `oom_handler` 时，`Frame` 枚举包含四种状态：

| 状态 | 含义 |
|------|------|
| `InMemory(Arc<FrameTracker>)` | 物理页常驻 |
| `Compressed(Arc<ZramTracker>)` | 内容在 zram |
| `SwappedOut(Arc<SwapTracker>)` | 内容在 swap |
| `Unallocated` | 尚未分配 |

`VmPageStore` 还维护：

| 字段 | 作用 |
|------|------|
| `active: VecDeque<VirtPageNum>` | 可回收页队列 |
| `compressed` | 压缩页计数 |
| `swapped` | 换出页计数 |

页被分配进 `VmPageStore` 时会 `record_active(vpn)`。

`VmPageStore` 在启用 OOM 时保存 active 队列和压缩/换出计数：

```rust
pub struct VmPageStore {
    pub vpn_range: VPNRange,
    frames: BTreeMap<VirtPageNum, Frame>,
    #[cfg(feature = "oom_handler")]
    active: VecDeque<VirtPageNum>,
    #[cfg(feature = "oom_handler")]
    compressed: usize,
    #[cfg(feature = "oom_handler")]
    swapped: usize,
}
```

active 队列只记录可回收候选；能否真正回收还要看 frame 当前状态、引用计数和后端 zram/swap 是否可用。

## 6. VMA 级回收

`Vma::do_oom()` 先尝试压缩，再尝试 swap。每轮只处理函数入口时已有的候选数：

```text
repeat initial_active_len times
  └── vpn = active.pop_front()
  ├── frame.zip()
  │     ├── 成功: unmap PTE, compressed += 1
  │     ├── SharedPage: 放回 active 队尾
  │     └── ZramIsFull -> 尝试 swap
  └── frame.swap_out()
        ├── 成功: unmap PTE, swapped += 1
        ├── SharedPage: 放回 active 队尾
        └── swap 不可用/满 -> stop
```

共享页不会被普通 zip/swap 回收：

`Frame::zip()` 和 `Frame::swap_out()` 只在 `Arc::strong_count(frame_ref) == 1` 时继续执行；引用计数大于 1 时返回 `MemoryError::SharedPage`。

B67 删除了绕过引用计数的 `force_swap()`/`force_swap_out()`。这类路径会把单个 VMA 的
resident backing 替换为 swap 状态，却让 futex 队列、SysV SHM 或 fork 的其它持有者继续
引用旧 frame；换入产生新 frame 后，共享对象会分裂。deep clean 现在只扩大候选 VMA 范围，
不再改变单页回收的所有权规则。

`SharedPage` 必须放回队尾而不是永久丢弃。futex queue 的 backing pin 是临时引用；waiter
离开空队列后 pin 会释放，后续 OOM 扫描应能重新考虑该页。扫描次数固定为入口队列长度，
避免同一轮反复取出仍被 pin 的页而死循环。

## 7. 地址空间级回收

`AddressSpace::do_shallow_clean()` 和 `do_deep_clean()` 按架构选择用户 mmap 范围。

rv64：

| 方法 | 范围 |
|------|------|
| shallow | `MMAP_BASE..TASK_SIZE` 且非文件映射 |
| deep | `TASK_SIZE` 以下全部非文件映射，统一使用 `do_oom()` |

la64：

| 方法 | 范围 |
|------|------|
| shallow | `USR_MMAP_BASE..USR_MMAP_END` 且非文件映射 |
| deep | `USER_VA_END` 以下全部非文件映射，统一使用 `do_oom()` |

文件映射不走这些匿名页回收路径。

## 8. 换入路径

缺页分类遇到 `Compressed` 或 `SwappedOut`：

| 状态 | FaultAction | 函数 |
|------|-------------|------|
| `Compressed` | `Decompress` | `finish_decompress_page()` |
| `SwappedOut` | `SwapIn` | `finish_swap_in_page()` |

恢复步骤：

1. 从 zram/swap 读取到新 frame。
2. `UserMapper::map_user_page()` 安装用户 PTE。
3. `vm_record_resident_page()` 记录 active。
4. 递减 compressed/swapped 计数。
5. 返回物理地址。

## 9. overcommit sysctl

`mm/sysctl.rs` 维护内存策略：

| 项 | 默认值 | 说明 |
|----|--------|------|
| `overcommit_memory` | `0` | 0/1/2 三种模式 |
| `overcommit_ratio` | `50` | commit limit 比例 |
| `max_map_count` | `65530` | 用户 VMA 数限制 |
| `min_free_kbytes` | `1024` | 导出/配置值 |
| `panic_on_oom` | `0` | 导出/配置值 |

`reported_memory_bytes()` 使用 `USABLE_MEMORY_SIZE`；QEMU 构建对外报告仍截断到
`512 MiB`，2K1000LA 当前报告 `2043852 KiB`，不把第 0 页和临时固件 carveout
计入可用内存。`MEMORY_SIZE` 仍表示板载 2 GiB 总容量。`commit_limit_bytes()` 继续
被 `64 MiB` 上限截断，避免测试按总内存放大提交压力。

## 10. overcommit_allows()

```rust
match overcommit_memory() {
    1 => true,
    2 => current + additional <= commit_limit_bytes(),
    _ => additional <= reported_memory_bytes(),
}
```

完整 sysctl 状态和判断函数位于 `mm/sysctl.rs`：

```rust
const REPORTED_MEMORY_CAP_KB: usize = 512 * 1024;
const COMMIT_LIMIT_CAP_KB: usize = 64 * 1024;
const DEFAULT_OVERCOMMIT_MEMORY: usize = 0;
const DEFAULT_OVERCOMMIT_RATIO: usize = 50;
const DEFAULT_MAX_MAP_COUNT: usize = 65_530;
const DEFAULT_MIN_FREE_KBYTES: usize = 1_024;

static OVERCOMMIT_MEMORY: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_MEMORY);
static OVERCOMMIT_RATIO: AtomicUsize = AtomicUsize::new(DEFAULT_OVERCOMMIT_RATIO);
static MAX_MAP_COUNT: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_MAP_COUNT);
static MIN_FREE_KBYTES: AtomicUsize = AtomicUsize::new(DEFAULT_MIN_FREE_KBYTES);
static PANIC_ON_OOM: AtomicUsize = AtomicUsize::new(0);

pub fn commit_limit_bytes() -> usize {
    reported_memory_bytes()
        .saturating_mul(overcommit_ratio())
        .saturating_div(100)
        .min(COMMIT_LIMIT_CAP_KB.saturating_mul(1024))
}

pub fn overcommit_allows(current_committed_bytes: usize, additional_bytes: usize) -> bool {
    match overcommit_memory() {
        1 => true,
        2 => {
            current_committed_bytes
                .saturating_add(additional_bytes)
                <= commit_limit_bytes()
        }
        _ => additional_bytes <= reported_memory_bytes(),
    }
}
```

`REPORTED_MEMORY_CAP_KB` 和 `COMMIT_LIMIT_CAP_KB` 影响对用户态报告和 LTP tunable 压测规模。`overcommit_memory=0` 的判断只看本次 additional 是否超过 reported memory，不把 current committed 累加进去。

调用点：

| 调用点 | additional |
|--------|------------|
| `do_mmap()` | mmap len |
| `do_sbrk()` | heap 新增页长度 |

只有匿名可写 mmap 计入 overcommit。文件映射和只读匿名映射不在 `charges_overcommit()` 中计费。

## 11. max_map_count

`VmaSet::ensure_can_add()` 用 `max_map_count()` 限制用户 VMA 数。VMA 分裂时也会提前 `try_reserve(additional)`。

这意味着以下操作都可能因为 VMA 数量限制返回 `ENOMEM`：

| 操作 | 原因 |
|------|------|
| mmap 新段 | 需要新增 1 个 VMA |
| mprotect 中间范围 | 可能把 1 段分成 3 段 |
| madvise fork 标记 | 可能分裂 VMA |
| munmap 中间范围 | 分裂后删除目标段 |

## 12. locked pages

locked pages 由 `AddressSpace.locked_pages: BTreeSet<VirtPageNum>` 维护。

接口行为：

| 接口 | 行为 |
|------|------|
| `mlock(start, len)` | 检查范围，逐页 fault-in，然后标记 |
| `mlock_onfault(start, len)` | 检查范围，只标记 |
| `munlock(start, len)` | 清除标记 |
| `mlockall_current()` | 标记所有用户 VMA 页 |
| `munlockall()` | 清空 |

`user_lock_range()` 要求范围被用户 VMA 完整覆盖，并且结束地址不超过 `USER_VA_END`。

## 13. locked pages 与 madvise/msync

锁页会影响部分内存操作：

| 操作 | 约束 |
|------|------|
| `madvise(MADV_DONTNEED)` | 范围内有 locked page 返回 `EINVAL` |
| `validate_msync_range(invalidate = true)` | 范围内有 `MAP_LOCKED` VMA 返回 `EBUSY` |
| `munmap` | 成功后清除范围内 locked pages |
| `MAP_FIXED` 覆盖 | 覆盖前清除旧 locked pages |

locked page 只是地址空间元数据；是否常驻取决于是否已经 fault-in。`mlock_onfault` 和 `MAP_LOCKED` 不立即安装 PTE。

## 14. 防御性限制

MM 中多个限制用于避免 OOM 扩散：

| 限制 | 位置 |
|------|------|
| `MAX_BUFFER_SIZE = 8 MiB` | `uaccess.rs` |
| `MAX_IOVEC_COUNT = 1024` | `uaccess.rs` |
| `MAX_EAGER_MMAP_SIZE = 1 GiB` | `mmap.rs` writable anonymous shared |
| ELF LOAD 段 `mem_size > 1 GiB` 拒绝 | `address_space.rs::map_elf()` |
| `try_reserve()` 失败返回 `ENOMEM` | VMA/vector 构造路径 |

这些不是临时绕过，而是 syscall 兼容和裸机内核资源上限下的明确防线。

## 15. 关键约束

1. 内核堆分配失败不能通过调度当前任务退出解决。
2. `frame_reserve()` 只在 `oom_handler` 特性下有效；未启用时不能依赖其释放内存。
3. shared page 默认不能被普通 OOM zip/swap，因为 `Arc` 引用数大于 1。
4. locked pages 不等于 resident pages。
5. overcommit 只限制承诺量，不代表已经分配物理页。
6. `max_map_count` 约束 VMA 数，不约束页数。

OOM 相关代码要分清“承诺量”“驻留量”和“可回收量”。`mmap` overcommit 关注未来可能使用的虚拟承诺；resident pages 表示已经有 frame；locked pages 只是标记不能被某些回收/advice 路径丢弃；shared PageCache 和 shared anonymous 页因为引用复杂，不能简单当作当前进程私有内存回收。

锁顺序同样是 OOM 路径的风险点。回收可能触碰 VMA、PageCache、frame store 和调度状态，不能在持有业务锁时进入可能等待或分配的路径。文档中的“锁 -> clone Arc -> 释放锁 -> 操作”规则，在 mmap、filemap、PageCache reclaim 和 wait queue 交互处都适用。

panic 诊断是更严格的不可等待上下文。`heap_stats()` 与 `unallocated_frames()` 保留给普通
调用者的阻塞语义；`panic_diag` 只能使用 `try_heap_stats()` / `try_unallocated_frames()`，
锁忙时退化输出，禁止在 allocator 临界区 panic 后递归等待同一把锁。

## 16. 调试核对点

| 现象 | 检查 |
|------|------|
| 分配失败后直接 shutdown | 是否已进入 `handle_alloc_error()`，堆 recovery 是否失败 |
| 有空闲物理页但 heap OOM | `heap_stats()` 的内部浪费与 free bytes |
| mmap 返回 ENOMEM | overcommit、mmap hole、max_map_count、页帧余量分别排查 |
| mlock 后 mincore 仍为 0 | 是否使用 `mlock_onfault` 或 `MAP_LOCKED`，它们不立即 fault-in |
| OOM recovery 没有回收文件页 | 当前回收路径过滤了 file-backed VMA |
