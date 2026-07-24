# Bug: la64 内核栈溢出导致堆内存损坏（clone09 panic）

## 概述

- **日期**: 2026-06-08
- **严重程度**: 高（静默内存损坏 → 不确定位置的 panic）
- **影响范围**: la64 架构，涉及 CLONE_NEWNET / 深调用链的场景
- **触发条件**: 单独跑 LTP clone09 即可触发

## 现象

LTP `clone09` 测例 **本身 PASS**，但随后内核 panic：

```
[kernel] panicked at 'called `Option::unwrap()` on a `None` value',
/rustc/f705de59625bb76067a5d102edc1575ff23b8845/library/alloc/src/collections/btree/navigate.rs:535:47

--- SYSCTX ---
syscall: yield
--- TASK ---
no current task
```

## 根因

**la64 内核栈是堆分配的 `Vec<u8>`，没有 guard page。栈溢出时不会触发缺页异常，而是静默写入相邻堆内存，破坏碰巧紧邻的 BTree 内部节点。**

### 两架构对比

| | rv64 (正常) | la64 (有 bug) |
|---|---|---|
| 栈大小 | `PAGE_SIZE * 0x20` (128KB) | `PAGE_SIZE * 0x10` (64KB) |
| 分配方式 | 独立页表映射 | `Vec::alloc`（堆分配） |
| 隔离保护 | 栈之间夹 1 个 guard page（未映射） | **无** |
| 栈溢出后果 | 访问 guard page → 缺页异常 → 可检测 | 写入下一个堆块 → 静默内存损坏 |

### 代码路径

```
la64: os/src/hal/arch/loongarch64/kern_stack.rs:21
  Self(alloc::vec![0_u8; KERNEL_STACK_SIZE])   // 64KB，malloc 出来的，无保护

rv64: os/src/hal/arch/riscv/kern_stack.rs:14-15
  let top    = TRAMPOLINE - kstack_id * (KERNEL_STACK_SIZE + PAGE_SIZE);  // + PAGE_SIZE = guard
  let bottom = top - KERNEL_STACK_SIZE;
  KERNEL_SPACE.lock().insert_framed_area(bottom, top, R|W);
  // guard page 不映射，溢出 → 缺页
```

### 触发链

1. `sys_clone(CLONE_NEWNET)` → `NetNamespace::new_isolated()` → `NetDeviceEntry::new()`
2. `NetDeviceEntry::new()` 创建 dummy smoltcp 对象（`Loopback::new`、`Config::new`、`Interface::new`、`SocketSet::new`），这些临时变量压在栈上
3. 叠加正常的 clone 路径开销（PCB 创建、FD 表复制、VM 设置等），栈用量超过 64KB
4. 溢出写入紧邻栈的堆内存 → 恰好是 `PROCESS_SHARED_FUTEX` 这个 BTreeMap 的一个内部节点
5. 后续 `run_tasks()` 循环调用 `compact_shared_futex()` → `BTreeMap::retain()` 遍历时访问坏节点 → `perform_next_checked()` 中的 `next_kv().ok().unwrap()` panic

### 关于 NetDeviceEntry 的 dummy smoltcp 对象

`os/src/net/net_core.rs:107-109`：

```rust
// --- smoltcp dummy context (satisfies Iface trait interface) ---
smoltcp_iface: Mutex<Interface>,
sockets: Mutex<SocketSet<'static>>,
```

- **来源**：commit `027f0df`（Panpeach，2026-06-02，"Round 2 veth/netlink/netns rewrite"）
- **目的**：NetDeviceEntry 需要实现 `Iface` trait，而 trait 要求 `fn common(&self) -> &IfaceCommon` 和 `fn as_smoltcp_device(&self) -> &dyn SmoltcpDeviceAccess` 返回引用。Rust 要求引用必须指向实际存储的数据，所以需要这两个 dummy 字段。
- **实际使用情况**：这两个方法对 NetDeviceEntry 的实现都是 **直接 panic**（第 223/229 行），从未被调用。真正的协议处理走 `NetInterface` 的另一条路径。
- **在本次 bug 中角色**：导火索——它们的构造过程（特别是 `Loopback::new`、`Interface::new`）在栈上产生较大的临时对象，让 64KB 栈不够用。但就算没有它们，换一个别的深调用链同样会爆，根本原因是栈没有隔离保护。

## 临时缓解（已做）

`os/src/hal/arch/loongarch64/config.rs:34`：`KERNEL_STACK_SIZE` 从 `PAGE_SIZE * 0x10`（64KB）提到 `PAGE_SIZE * 0x20`（128KB）。

⚠️ **副作用**：`SYSTEM_TASK_LIMIT` 从 1024 降到 512（因为 `by_heap = 128M / (128K * 2) = 512`），可能影响高并发 fork/futex 测试。

## 建议的长期修复

### 方案 A：给 la64 做独立页表映射 + guard page（治本）

参照 rv64 的 `kern_stack.rs`，把 la64 的栈管理改为：
1. 预分配一段虚拟地址空间
2. 每个栈用独立的物理页映射，栈之间夹 guard page
3. 在 `KernelStack::drop` 时回收物理页

涉及改动：
- `os/src/hal/arch/loongarch64/kern_stack.rs`（重写）
- `os/src/hal/arch/loongarch64/config.rs`（可能需要调整地址布局）
- 页表管理（确认 LA64 TLB 操作兼容，`invtlb` 刷新）

### 方案 B：精简 NetDeviceEntry 去掉 dummy smoltcp（降低栈压力）

把 `smoltcp_iface` 和 `sockets` 字段从 `NetDeviceEntry` 中移除。这需要调整 `Iface` trait 的 `common()` 和 `as_smoltcp_device()` 方法（例如改为返回 `Option`，或把 NetDeviceEntry 拆成单独的 trait）。

好处：减小栈压力 + 节省堆内存 + 可能允许保持较小栈（64KB），从而保持 `SYSTEM_TASK_LIMIT = 1024`。

### 建议优先级

先做 A（治本），再做 B（优化）。如果 A 实施困难（LA64 DMW 架构限制），则 A+B 并行考虑。

## 验证方法

```bash
# 单独跑 clone09，确认不再 panic
# rv64（参照，应该一直正常）
make rv64-kernel-build-only && make rv64-run  # mask=0x001, ltp_include=clone09

# la64（修复后应正常）
make la64-kernel-build-only && make la64-run  # mask=0x001, ltp_include=clone09
```

## 复现步骤（修复前）

```bash
# 进入 Docker
make docker

# 编译 la64
cd os && make la64-kernel-build-only

# 运行 clone09
# (配置 os_test.conf 为 mask=0x001, ltp_include=clone09, ltp_runner=inline)
make la64-run
```

---

## 2026-06-18 更新

### 临时缓解

将 LA64 `KERNEL_STACK_SIZE` 从 `PAGE_SIZE * 0x10` (64KB) 恢复为 `PAGE_SIZE * 0x20` (128KB)，确保 LTP `clone09` (`CLONE_NEWNET`) 不再触发栈溢出。

### Pending: 根治 CLONE_NEWNET 路径栈压力

`clone09` 即使有 128KB 也有风险，因为 `sys_clone` → `NetNamespace::new_isolated()` → `NetDeviceEntry::new()` 会在内核栈上构造 smoltcp 大临时对象（`Loopback` + `Interface` + `SocketSet`）。

**待优化方向：**
- 将 `NetDeviceEntry::new()` 中的 smoltcp 对象改为堆分配（`Box` / `Arc`）
- 或拆分 `new_isolated()` 为更细粒度的阶段，减少单次调用栈深度

**验证状态：**
- 2026-06-18: 64KB 栈 `clone09` 必现溢出 → 临时调至 128KB 绕过
- 128KB 是否足够其他深调用场景（LTP 全量）：待跑全量验证

---

## 2026-07-10 更新：2K1000LA 实板栈窗口必须符合 40 位 VALEN

guard page 映射栈在 QEMU 上验证有效，但最初直接把 QEMU 栈窗口用于 2K1000LA，导致首个任务创建时访问栈顶即 panic：

```text
[bringup][kstack:01] ... probe=0xffffff7fffffeff8 ...
Exception(AddressError), bad addr=0xffffff7fffffeff8, subcode=1
```

实板 `CPUCFG1=0x03e2727e`，按 LoongArch `PABITS=[11:4]`、`VABITS=[19:12]` 解码后，`PALEN=VALEN=40`。对于 40 位虚拟地址，`0xffffff8000000000` 是第一个合法高半地址；旧 `KERNEL_STACK_TOP=MMAP_BASE-PAGE_SIZE` 位于它下方，因此属于非规范地址。该异常发生在页表查询之前，与 PGDH、PTE、TLB refill 或 `__switch` ABI 无关。

修复后的平台布局：

| 平台 | PALEN / VALEN | kernel stack window |
|------|---------------|---------------------|
| LA64 QEMU | 48 / 48 | `MMAP_BASE - PAGE_SIZE` 向下 |
| 2K1000LA | 40 / 40 | `MMAP_END - PAGE_SIZE` 向下 |

两条路径都保留 128KiB 映射栈和每 slot 一个 guard page。实板首个栈探针只执行一次；预期先出现 `kstack:01`，随后出现 `kstack:02`，再进入 `sched:01` 和 `user:01`。

调试时应先根据异常类别分流：`AddressError` 优先检查 `CPUCFG1`、地址规范性和平台常量；只有 `PageInvalid*`、TLB refill 等异常才进入 PGDH/PTE/TLB 排查。

### VALEN=40 全链路审计结论

并行审计覆盖 `VA_MASK/SEG_MASK`、VPN/VPPN 截取、PTE PPN、TLB refill、ASID、DMW 和临时内核映射边界。结论是首栈顶 `0xfffffffffffef000` 本身正确，1024 个 `128KiB + 4KiB guard` slot 的完整窗口 `0xfffffffff7bef000..0xfffffffffffef000` 均为 40 位 canonical 高地址。

审计同时发现并修复了以下关联缺陷：

1. `STLBPS`/`TLBREHI.PS` 曾错误使用 `PTE_WIDTH_BITS=3`；4KiB 页必须写 `12`。
2. LAFlex PTE 的 PPN 掩码曾多左移 12 位；现按 `PA[PALEN-1:12]` 截取。
3. TLBEHI 写入高地址 VPPN 前未裁剪字段宽度，会触发 `bit_field` 越界断言；读回也缺少 paired-page 左移和 VPN 符号扩展。
4. 临时内核 ELF 映射过去没有上界，可能覆盖实板栈窗口或 QEMU 下低 39 位同址的页表别名；现由 `KERNEL_PROGRAM_END` 拒绝越界。
5. `__restore` 曾把只读 `ASIDBITS` 混入 ASID，并在 PGDL 不变时跳过 ASID 更新；ASID 分配耗尽哨兵也可能被写成 10-bit ASID 1023。现分别比较 PGDL/ASID并成对更新，耗尽时安全退化到 ASID 0。
6. 2K1000 PCI ECAM/AHCI CPU 访存改走 DMW2 SUC 别名，避免把 `0xfe00000000` 当作非 canonical 普通 VA 或以 cached 属性访问 MMIO。

构建期断言固定了 2K1000 的 `VALEN/PALEN=40/40`、首栈顶/窗口下界、VA/VPN 符号扩展和 39-bit 软件页表别名边界。启动期在 `mm::init()` 前再次用 `CPUCFG1` 校验硬件位宽，并强制 `RVACFG.RBits=0`。

仍需单独处理的风险不属于本次 VALEN 修复：kernel stack slot 耗尽目前仍 panic；临时 ELF 映射失败路径还缺少完整 RAII 回滚；单页 guard 无法拦截一次跨越超过 4KiB 的大幅栈指针跳转。它们不影响当前首栈地址合法性结论，但应在压力回归前继续修复。
