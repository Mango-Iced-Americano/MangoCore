---
title: "MM 初始化与内核地址空间"
category: mm
status: stable
author: MangoCore Team
last_update: 2026-08-04
tags: [mm, init, kernel-space, mapping, smp]
code_paths:
  - "os/src/mm/mod.rs"
  - "os/src/mm/frame_allocator.rs"
  - "os/src/mm/kernel_space.rs"
  - "os/src/hal/firmware/"
---

# MM 初始化与内核地址空间

## 1. 入口位置

MM 初始化入口位于 `os/src/mm/mod.rs`，由 `os/src/main.rs::rust_main()` 在日志初始化后调用。启动顺序中，内存管理位于驱动、文件系统、网络和任务初始化之前：

```rust
console::log_init();
trace::init();
mm::init();
machine_init();
task::timer_cpu_init();
```

因此，内核堆、物理页分配器和内核页表必须在设备初始化、文件系统挂载和 `initproc` 加载之前可用。

`mm::init()` 的实现顺序为：

```rust
pub fn init() {
    heap_allocator::init_heap();
    #[cfg(feature = "heap_trace")]
    heap_trace::enable();
    frame_allocator::init_frame_allocator();
    KERNEL_SPACE.lock().activate();
}
```

这个顺序体现了三个依赖关系：

1. 堆分配器先建立，后续 `Arc`、`Vec`、`BTreeMap` 等内核对象才能创建。
2. 物理页分配器随后建立，用于页表页、用户页、内核栈和文件页缓存。
3. `KERNEL_SPACE` 最后激活，使处理器使用内核页表运行后续初始化。

## 2. 模块边界

`os/src/mm/mod.rs` 负责导出架构无关 MM 接口，并通过 HAL 注入架构页表类型。

| 导出项 | 来源 | 作用 |
|--------|------|------|
| `PageTableImpl` | `hal` | 当前架构的用户页表实现 |
| `KernelPageTableImpl` | `hal` | 当前架构的内核页表实现 |
| `PageTable` | `mm/page_table.rs` | 架构无关页表 trait |
| `AddressSpace` | `mm/address_space.rs` | 进程地址空间 |
| `MapPermission`, `MapFlags` | `mm/vma.rs` | VMA 权限与 mmap 标志 |
| `frame_alloc`, `frame_dealloc` | `mm/frame_allocator.rs` | 物理页分配接口 |
| `FrameTracker` | `mm/frame_allocator.rs` | 页帧 RAII 句柄 |
| `KERNEL_SPACE`, `kernel_token` | `mm/kernel_space.rs` | 全局内核地址空间 |
| `translated_*`, `UserBuffer*` | `mm/uaccess.rs` | 用户地址访问封装 |
| `tlb_invalidate` | `hal` | 架构相关 TLB 刷新 |

架构差异不在 `mm/mod.rs` 内硬编码。rv64 由 SV39 实现页表，la64 由 LoongArch64 flexible page table 实现页表；`AddressSpace<T: PageTable>` 只依赖 trait。

## 3. 初始化依赖图

```
rust_main()
  ├── console::log_init()
  ├── trace::init()
  ├── mm::init()
  │     ├── heap_allocator::init_heap()
  │     ├── heap_trace::enable()            [feature = heap_trace]
  │     ├── frame_allocator::init_frame_allocator()
  │     └── KERNEL_SPACE.lock().activate()
  ├── machine_init()
  ├── task::timer_cpu_init()
  ├── drivers / fs / net
  ├── task::add_initproc()
  └── task::run_tasks()
```

`machine_init()` 在 MM 初始化之后执行。rv64 的 `machine_init()` 设置 trap 和 timer interrupt；la64 的 `bootstrap_init()` 会在更早阶段配置部分架构寄存器，但 `machine_init()` 同样在 `mm::init()` 之后完成 trap/timer 初始化。

## 4. 内核堆初始化

内核堆位于 `os/src/mm/heap_allocator.rs`，静态数组 `HEAP_SPACE` 的大小由 `hal::KERNEL_HEAP_SIZE` 决定：

```rust
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];
```

`init_heap()` 将该数组交给 `KernelAllocator` 的内部 buddy heap（`MetadataHeap<32, 12>`）和 slab 分配器：

```rust
HEAP_ALLOCATOR.init(HEAP_SPACE.as_ptr() as usize, KERNEL_HEAP_SIZE);
KERNEL_HEAP_CURRENT_BYTES.store(0, Ordering::Relaxed);
KERNEL_HEAP_MAX_BYTES.store(0, Ordering::Relaxed);
```

`KernelAllocator` 组合了 slab 分配器（9 个 size class: 8~2048 bytes）和 `MetadataHeap<32, 12>` buddy allocator。小对象走 slab，大对象直接走 buddy。分配失败时仍支持 OOM recovery 路径。

SMP 下仍只有一个全局堆，但并发访问由 `KernelAllocator.inner` 的 `Mutex` 串行化。
`KernelHeapInner` 独占 buddy 与 `SlabAllocator`，所有 slab 操作都要求 `&mut self`；因此
只需要证明顶层 `SlabAllocator: Send`，使其能够被全局 `Mutex` 持有。内部
`SlabPage`、`SlabList`、`SlabCache` 和分配器本身均不声明 `Sync`，避免把“运行期有锁”
错误扩大成“任意共享引用都可跨 CPU 并发访问”的类型授权。`HEAP_SPACE` 则只在单次启动
初始化时以裸地址交给该分配器，之后不再通过静态数组名称访问。

可选的 `heap_trace` 也只使用一个全局锁，但它不再让锁中状态通过裸指针指向两张
`static mut` 表。`TRACE: Mutex<TraceState>` 直接拥有 active/site 定长表，因此可变
引用的生命期被 mutex guard 约束，`TraceState` 的 `Send` 也由字段类型自动推导。
约 25.6 MiB 的全零表显式放在 `.bss.heap_trace`；它位于 `sbss..ebss` BSP 清零区，
不占用 raw kernel image 的文件负载。

### 4.1 分配失败路径

`GlobalAlloc::alloc()` 最多尝试三次：

1. 锁住 buddy heap 并尝试 `inner.alloc(layout)`。
2. 成功时记录堆分配 perf 统计和当前/峰值字节数。
3. 失败时释放 heap 锁，调用 `recover_for(layout)`。
4. 如果 recovery 无法释放足够内存，返回 null pointer，由 Rust 分配错误路径处理。

`handle_alloc_error()` 不调度当前任务退出，而是打印诊断信息并调用 `hal::shutdown()`。源码注释给出了原因：分配错误处理函数是发散函数，如果在 syscall handler 栈上直接切换任务，栈上的锁守卫无法析构，可能导致死锁或文件系统损坏。

## 5. 物理页分配器初始化

物理页分配器位于 `os/src/mm/frame_allocator.rs`。初始化时遍历 BSP 已冻结的
`hal::firmware::memory_regions()`，并扣除第 0 页、`[skernel, ekernel)` 与合并后的
固件保留区：

```rust
for_each_usable_frame_region(|start, end| {
    regions.push(FrameRegion::new(start.0, end.0));
});
```

页帧粒度为 `PAGE_SIZE`。`FrameTracker::new(ppn)` 会将整页按 `u64` 清零，保证新分配的普通页不会泄露旧内容。`frame_alloc_uninit()` 只在明确需要未初始化页的路径使用，例如 COW 中先分配再整页复制。

## 6. KERNEL_SPACE 全局对象

`KERNEL_SPACE` 位于 `os/src/mm/kernel_space.rs`：

```rust
lazy_static! {
    pub static ref KERNEL_SPACE: Arc<Mutex<KernelSpace<KernelPageTableImpl>>> =
        Arc::new(Mutex::new(KernelSpace::new()));
}
```

`KernelSpace<T>` 包含：

| 字段 | 含义 |
|------|------|
| `page_table: T` | 内核页表 |
| `kernel_mappings: BTreeMap<VirtPageNum, KernelMappingArea>` | 由 `FrameTracker` 管理的内核动态映射 |

`kernel_token()` 直接返回 `KERNEL_SPACE.lock().page_table.token()`，供内核访问和用户地址翻译路径引用。

## 7. 内核固定映射布局

`KernelSpace::new()` 构造内核地址空间时会依次建立固定映射。

| 区间 | 权限 | 来源 |
|------|------|------|
| trampoline | `R | X | G` | `strampoline`，仅在 `should_map_trampoline!()` 为真时映射 |
| `.text` | `R | X | G` | `stext..etext` |
| `.rodata` | `R` | `srodata..erodata` |
| `.data` | `R | W | G` | `sdata..edata` |
| `.bss` | `R | W | G` | `sbss_with_stack..ebss` |
| usable DRAM regions | `R | W | G` | 运行期 FDT/实板 fallback region 扣除内核和固件 carveout 后的各区间 |
| MMIO | `R | W | G` | FDT 早期 MMIO 资源（含 PCI host ECAM 与 memory window） |

映射通过 `kernel_identical_map!` 宏建立。这里的“identical”指虚拟页号和物理页号一致。FDT
MMIO 资源的对齐中段优先使用 2 MiB 映射，避免把 QEMU PCI host 的大 64-bit memory window
逐个拆成 4 KiB PTE；首尾未对齐部分仍用 4 KiB 映射。
RV64 为每个动态 RAM 页建立真实叶子 PTE；LA64 的低地址恒等访问由 DMW 提供，并按固件
最高 DRAM 地址建立软件 dirty bitmap。2K1000LA 和 LA64 QEMU 的多个 bank 分别处理，
中间 MMIO 空洞不会作为普通内存映射、清零或分配。

## 8. 动态内核映射

内核地址空间还提供几类动态插入接口：

| 接口 | 用途 |
|------|------|
| `insert_framed_area()` | 分配一段内核虚拟区并映射新物理页 |
| `insert_kernel_stack_area()` | 为内核栈创建映射 |
| `insert_program_area()` | 为加载到内核空间的程序/解释器数据建立映射 |
| `remove_area_with_start_vpn()` | 按起始 VPN 删除映射并释放 `FrameTracker` |

这些接口共同依赖 `KernelMappingArea` 保存 `vpn_range`、权限和物理页帧。删除映射时，`FrameTracker` drop 会归还页帧。

## 9. 回滚语义

动态映射在多页插入时可能中途失败。`kernel_space.rs` 的插入路径会记录已经映射的页，并在后续失败时反向 unmap，避免出现 VMA 元数据未插入但页表已部分生效的状态。

典型流程为：

```
allocate frame
  ├── map page
  ├── push mapped vpn
  └── on error:
        ├── unmap mapped vpns
        └── drop allocated frames
```

这类回滚路径是 MM 文档需要保留的实现细节，因为它解释了为什么内核映射插入接口返回 `Result` 而不是直接 panic。

## 10. 激活内核页表

`mm::init()` 最后执行：

```rust
KERNEL_SPACE.lock().activate();
```

`activate()` 是 `PageTable` trait 的方法，由具体架构页表实现：

| 架构 | 页表实现 | activate 语义 |
|------|----------|---------------|
| rv64 | `Sv39PageTable` | 写入页表 token 并刷新地址转换状态 |
| la64 | `LAFlexPageTable` | 切换 LoongArch64 页表相关寄存器并刷新转换状态 |

MM 文档不把寄存器细节放在本页展开；架构相关页表实现和 TLB 行为由 `page-table-and-tlb.md` 与 `docs/01_architecture/` 交叉说明。

## 11. 与进程地址空间的关系

内核地址空间和用户地址空间在职责上分离：

| 项目 | 内核地址空间 | 用户地址空间 |
|------|--------------|--------------|
| 类型 | `KernelSpace<KernelPageTableImpl>` | `AddressSpace<PageTableImpl>` |
| 生命周期 | 内核全局单例 | 每进程一份 |
| 固定映射 | 内核段、物理内存、MMIO、trampoline | ELF、heap、mmap、stack、trap context |
| 页帧管理 | `KernelMappingArea` 持有 `FrameTracker` | `Vma.inner: VmPageStore` 持有或引用页帧 |
| 激活时机 | `mm::init()` | 任务切换、exec、fork 后使用 |

用户态的 trampoline 和 signal trampoline 由 `AddressSpaceInner::from_elf()` 和 `AddressSpaceInner::from_existing_user()` 单独映射，不属于 `KERNEL_SPACE.kernel_mappings`。

## 12. 关键约束

1. `mm::init()` 必须在文件系统和任务初始化之前完成。
2. `heap_allocator::init_heap()` 必须早于任何需要堆分配的 MM 数据结构。
3. `frame_allocator::init_frame_allocator()` 的起始页来自 `ekernel`，不能覆盖内核镜像。
4. 动态内核映射失败时必须回滚已映射页。
5. `KERNEL_SPACE.lock().activate()` 后，后续内核代码依赖页表映射、TLB 和架构 MMU 状态一致。
6. 内核 PTE 的安全接口必须由底层实现配套 TLB 刷新；用户页表的 raw/no-flush
   原语只能由 `MmuGather` 调用并统一提交。B21 已为动态 kernel-global 映射实现锁外全核
   shootdown 和 ack 后 frame/slot 回收；B22/B23 已把用户 MM 的激活、generation、
   全用户 IPI/ack 和 PTE/frame 锁外提交闭合到 `AddressSpace`。

内核地址空间和用户地址空间的区别决定了调试方向。内核段、物理内存 direct map、MMIO 和 trampoline 属于 `KERNEL_SPACE`；ELF、heap、mmap、用户栈、trap context 属于每个进程自己的 `AddressSpace`。驱动访问 MMIO 失败不应去查用户 VMA；用户态 page fault 也不应从 `KERNEL_SPACE` 的映射表里找原因。

`mm::init()` 之后内核才能依赖堆和 frame allocator，因此早期初始化代码必须保持简单。若在 `mm::init()` 前引入会分配 `Vec/Arc` 的路径，可能在日志可见之前就破坏启动；若 frame allocator 起始地址算错，则会覆盖内核镜像或漏掉可用物理页。

## 13. 调试核对点

| 现象 | 优先检查 |
|------|----------|
| `mm::init()` 前后 panic | 堆初始化是否早于 `Vec/Arc/BTreeMap` 创建 |
| 早期页故障 | 内核段、usable DRAM region 或 MMIO 映射是否缺失；是否误把地址空洞当 RAM |
| 驱动初始化访问 MMIO 失败 | FDT 早期 MMIO 资源是否被 `KernelSpace::new()` 映射 |
| fork/exec 后用户态异常 | 用户地址空间映射，不应从 `KERNEL_SPACE` 查找 |
| 删除内核映射后悬空访问 | `remove_area_with_start_vpn()` 是否提前释放了仍在使用的映射 |
