---
title: "2K1000LA 首次上下文切换：40 位 VALEN、内核栈与 TLB 全链路复盘"
category: debug
status: resolved
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, valen, palen, kernel-stack, tlb, pte, asid, dmw]
code_paths:
  - "os/src/hal/arch/loongarch64/config.rs"
  - "os/src/hal/arch/loongarch64/kern_stack.rs"
  - "os/src/hal/arch/loongarch64/laflex.rs"
  - "os/src/hal/arch/loongarch64/tlb.rs"
  - "os/src/hal/arch/loongarch64/trap/mod.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "os/src/hal/arch/loongarch64/register/base/rvacfg.rs"
  - "os/src/hal/arch/loongarch64/register/mmu/tlbehi.rs"
  - "os/src/hal/arch/loongarch64/register/mmu/tlbrehi.rs"
  - "os/src/mm/kernel_space.rs"
  - "os/src/drivers/block/sata_blk.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/development-log.md"
  - "docs/09_debug/la64_on_board/260710/01-uimage-entry-and-platform-isolation.md"
  - "docs/09_debug/bug-la64-kernel-stack-overflow.md"
  - "docs/01_architecture/loongarch64-platform.md"
  - "docs/04_mm/page-table-and-tlb.md"
entry_points:
  - "kstack_alloc"
  - "KernelStack::new"
  - "__switch"
  - "trap_return"
  - "__restore"
  - "__rfill"
  - "LAFlexPageTable::activate"
---

# 2K1000LA 首次上下文切换：40 位 VALEN、内核栈与 TLB 全链路复盘

## 1. 一句话结论

首个任务停在 `sched:01` 后不是 `__switch` ABI、ELF 或 PTE 内容错误，而是从 QEMU
照搬的内核栈地址 `0xffffff7fffffeff8` 对 2K1000LA 的 40 位 VALEN 不规范，CPU 在
查询 PGDH/PTE/TLB 之前就抛出 `AddressError`；迁移整个 guarded stack 窗口后，又按
同一 40 位地址契约联审并修复了 PS、PPN、VPPN、ASID/PGDL、低 39 位页表别名和
DMW/MMIO 等潜在阻断点，最终实板进入 PLV3 initproc。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 触发 | 在 VALEN=40 的 2K1000LA 上恢复首个任务的 QEMU 风格高地址内核栈 |
| 最后一条正常探针 | `[bringup][sched:01]`，TCB 已创建，resume PC/SP 已准备 |
| 决定性异常 | `Exception(AddressError), bad addr=0xffffff7fffffeff8` |
| 真正故障层 | 虚拟地址规范性检查，位于页表查询/TLB refill 之前 |
| 直接根因 | 构建常量仍按 QEMU VALEN=48；旧栈位于 40 位高半区起点正下方 |
| 根因修复 | 2K1000 固定 `VALEN=PALEN=40`，栈窗口迁到 `MMAP_END` 下方合法高半区 |
| 防回归 | CPUCFG 启动校验、canonical helper、编译期窗口/掩码断言 |
| 关联审计 | TLB PS、PTE PPN、TLBEHI VPPN、ASID/PGDL、临时 ELF 别名、DMW2 MMIO |
| 首次完整实板 PASS | `4705b28d` 所含 Work_Log：高栈、首次切换、用户态入口均通过 |
| 不应混淆 | 2026-06 的 64 KiB 栈溢出是容量/guard 问题，不是本次 AddressError |

## 3. 必要底层原理

### 3.1 异常分层：AddressError 先于页表

一次普通页模式访存可按以下顺序理解：

```text
VA
-> 是否符合当前有效 VALEN 的 canonical form
-> 地址翻译模式（DMW 或页模式）
-> TLB search / refill
-> PGDL 或 PGDH 根页表
-> 多级 PTE、权限和 PALEN 范围
-> PA
```

`AddressError` 在第一层失败。`PageInvalid*`、`PageModify`、TLB refill 才说明请求已
进入地址翻译层。因此：

```text
mapped_frame(va) 能找到 PPN
```

只证明软件页表数据结构中有映射，不能证明该 VA 对硬件是合法输入。

### 3.2 40 位 canonical address 的严格公式

VALEN=40 时，有效虚拟地址的符号位是 bit 39。64 位寄存器中的 bits `[63:40]`
必须全部复制 bit 39：

```text
bit39 = 0: [0x0000000000000000, 0x0000007fffffffff]
bit39 = 1: [0xffffff8000000000, 0xffffffffffffffff]
```

中间区域不是“未映射但可缺页”的地址，而是非规范地址。硬件不会先去查 PTE。

### 3.3 CPUCFG1 是硬件真值

实板读到：

```text
CPUCFG1 = 0x03e2727e
PABITS  = CPUCFG1[11:4]  + 1 = 0x27 + 1 = 40
VABITS  = CPUCFG1[19:12] + 1 = 0x27 + 1 = 40
```

QEMU 报告 48/48。因此 `PALEN/VALEN` 不是“LoongArch64 统一常量”，而是平台能力；
QEMU 通过不能为实板高地址布局背书。`RVACFG.RBits` 还可能缩减有效 VA，启动时需
明确关闭 reduced-VA 模式并再校验。

### 3.4 软件页表只有低 39 位索引

当前三级页表每级 9 位、页偏移 12 位：

```text
PAGE_TABLE_VA_BITS = 12 + 9 * 3 = 39
```

PGDL/PGDH 选择低/高根，根以下只索引 `VA[38:12]`。因此两个仅在 bit 39 以上不同的
高地址可能在同一根页表中落到相同三级索引。固定高地址窗口迁移时，必须同时检查
其低 39 位别名，不能只看完整 64 位区间是否重叠。

### 3.5 TLB 表项是一对页，不是单页 VPN 的原样复制

LoongArch 一个 TLB 表项用 VPPN 标识偶/奇两个相邻页：

```text
VPN  = VA[VALEN-1:12]
VPPN = VA[VALEN-1:13] = VPN >> 1
```

写 `TLBEHI.VPPN` 时必须裁掉 VPN 的符号扩展，只写寄存器字段；读回时要补回被省略的
偶奇选择位并重新按 VPN 位宽符号扩展。把 `VA_MASK` 直接复用于 VPN/VPPN 会多保留
已经被页偏移移除的位数。

### 3.6 PS、PTE PPN 和 ASID 是三个独立编码

- `STLBPS.PS` 保存 `log2(page_size)`，4 KiB 应写 `12`；
- `TLBREHI` 包装函数接收字节数，应传 `4096` 后编码为 `12`；
- PTE 保存 `PA[PALEN-1:12]`，掩码不是“从 bit 12 开始再保留 PALEN 位”；
- `CSR.ASID[9:0]` 是当前 ASID，`[23:16]` 是只读的 ASID 位宽描述，两者不能 OR；
- PGDL 相同不意味着 ASID 相同，恢复用户态时两者必须分别比较并成对更新。

### 3.7 DMW CPU 别名和设备 DMA 地址不能互换

2K1000 PCI ECAM 物理地址 `0xfe00000000` 不能当作普通 40 位页模式 canonical VA
直接解引用。CPU 访问应使用 DMW2 强序非缓存别名；DMA 描述符仍写原始物理地址。
把 DMW 虚拟别名交给设备并不会让设备“沿 CPU 页表访问”，只会给硬件错误总线地址。

## 4. 按时间顺序的调试追溯

### 4.1 初始假设：卡在 init 任务创建或第一次 `__switch`

首轮日志只显示已进入调度前，可能范围很大：initramfs payload、ELF、TCB、用户栈、
内核栈、TaskContext、`__switch`、`trap_return` 都在范围内。直接读 panic 末尾不足以
判断是哪一层先失败。

为此增加仅在 2K1000 诊断构建中存在的分阶段探针：

```text
preload:01..18
tcb:01..11
sched:01/02
user:01..03
```

实板完成 TCB 创建并输出 `sched:01`，没有到 `user:01`。这把边界压缩为：

```text
__switch 恢复 TaskContext 的 ra/sp
-> 在新内核栈上执行 trap_return 第一条输出
```

ELF 解析、initramfs 解包、用户地址空间和任务入队均已越过，不再是首要嫌疑。

### 4.2 排除 `__switch` ABI：比较旧工程和恢复寄存器

诊断继续打印：

- 首个 TaskContext 的实际 resume PC；
- 预期 `trap_return` 地址；
- resume SP；
- 新内核栈 bottom/top、软件记录 PPN 和 PGDH。

上一届工程与当前 `switch.S` 相同，但旧工程使用 heap `Vec<u8>` 低地址栈；差异集中
在当前 VM-mapped 高地址栈。resume PC/SP 内容与预期一致后，继续把问题归因于
`__switch` 保存寄存器没有证据支持。

### 4.3 用启动栈探测新栈，把失败压到第一次高地址访存

内核仍在启动栈上时，对即将使用的新栈顶做一次 volatile 写回读。保留文档中的实板
原始诊断为：

```text
[bringup][kstack:01] ... probe=0xffffff7fffffeff8 ...
Exception(AddressError), bad addr=0xffffff7fffffeff8, subcode=1
```

`kstack:02` 没有出现，说明甚至不需要真的执行 `__switch`；第一次对新栈地址的 CPU
访存就失败。页表软件查询能找到映射与这一事实并不矛盾。

### 4.4 关键转折：先解码 CPUCFG，再看地址高位

实板 `CPUCFG1=0x03e2727e` 解出 40/40。旧实现硬编码 48/48，并使用：

```text
MMAP_BASE       = 0xffffff8000000000
old stack top   = MMAP_BASE - PAGE_SIZE
probe address   = 0xffffff7fffffeff8
```

`MMAP_BASE` 恰好是 40 位 canonical 高半区的第一个地址；旧栈就在其下方非规范区。
异常类别、CPUCFG 和数值边界三者完全相符，根因从“高栈 TLB 可疑”收敛为“高栈 VA
在进入 TLB 之前已经非法”。

### 4.5 修正栈后，不用一次通过掩盖地址链中的其他缺陷

只改 `KERNEL_STACK_TOP` 可以消除当前 `AddressError`，但不能证明后续 refill、ASID
切换和 MMIO 访问正确。于是对 VALEN/PALEN 全链路做审计，发现以下已提交缺陷：

| 子问题 | 修复前源码事实 | 风险 | 修复 |
|--------|----------------|------|------|
| STLB 页大小 | `STLBPS::set_ps(PTE_WIDTH_BITS)`，即写 3 | 把 PTE 大小当页大小 | 写 `PAGE_SIZE_BITS=12` |
| refill PS | `set_page_size(3)` 会由 `trailing_zeros` 编成 0；失败分支只 OR `0xc` | 旧 PS 位非零时 OR 不能覆盖 | 传 `PAGE_SIZE=4096`；refill 先清 `[5:0]` 再写 12 |
| PTE PPN | `((1<<PALEN)-1)<<12` | 掩码多延伸 12 位，混入保留/软件位 | `((1<<PALEN)-1) & !0xfff`，并断言 PPN 上界 |
| VPPN | 高 VPN 未裁字段；读回未补 paired-page 位/符号扩展 | CSR 越界或高地址搜索错误 | 独立 `VPN_MASK/VPPN_MASK` 与 canonicalize |
| 页表别名 | 临时内核 ELF 映射无架构上界 | 低 39 位索引可覆盖栈 PTE | `KERNEL_PROGRAM_END` 阻断别名区 |
| ASIDBITS | 将 `CSR.ASID[23:16]` OR 到进程 ASID | 把能力字段当标识符 | 只取 `[9:0]` |
| PGDL/ASID | PGDL 不变便跳过 ASID 更新 | 共享根或切换场景上下文陈旧 | 两者分别比较、连续写 CSR |
| ASID 耗尽 | 软件哨兵可能进入 CSR | 非法标识符/隔离失败 | 哨兵回退 ASID 0，并保守刷新 |
| 内核动态映射 | 仅按单一 global 假设刷新 | 复用栈 VA 命中旧非 global 项 | 当前实现对 kernel PT 修改全局刷新 |
| PCI/AHCI MMIO | 高 PA 当普通 VA | 非 canonical 或错误缓存属性 | CPU 使用 DMW2 SUC，DMA 保留 PA |

这里必须区分因果强度：**已由实板异常直接证明的首要根因只有非 canonical 栈
地址**。表中其余项是同一地址位宽迁移审计发现的独立正确性缺陷；它们会阻断或污染
后续启动，但没有原始日志证明每一项都曾单独触发该次 `AddressError`。

### 4.6 验证顺序：QEMU、反汇编、镜像、最后实板

提交 `b5826a65` 时完成：

- RV64/LA64 顺序编译；
- LA64 QEMU 报告硬件/构建 48/48 并进入 init 用户态；
- `__rfill` 目标文件反汇编确认先清 PS 再写 12；
- `__restore` 反汇编确认 PGDL/ASID 分别比较并连续写入；
- 2K1000 uImage Load/Entry 为 `0x90000000`。

但该提交记录明确说新镜像尚未在实板复测。随后 `4705b28d` 所含 Work_Log 才记录：
实板 `bootm` 成功进入 initproc，VALEN/PALEN、高栈探针、首次上下文切换和用户态入口
全部通过、无 panic。

## 5. 地址级根因证明

### 5.1 `bad addr` 为什么必然非规范

故障地址：

```text
A = 0xffffff7fffffeff8
```

取低 40 位：

```text
A[39:0] = 0x7fffffeff8
A[39]   = 0
```

若 bit 39 为 0，canonical 规则要求 bits `[63:40]` 全为 0；实际 A 的高位为 1。
因此 A 不是 40 位规范地址。它与第一个合法高半地址的距离是：

```text
0xffffff8000000000 - 0xffffff7fffffeff8 = 0x1008
```

这正对应旧栈页位于 `MMAP_BASE - 0x1000` 且探针再位于栈顶附近的布局。

### 5.2 新窗口为什么全部合法

当前 2K1000 栈参数：

```text
KERNEL_STACK_SIZE      = 0x20000       # 128 KiB
guard page             = 0x01000       # 4 KiB
KERNEL_STACK_SLOT_SIZE = 0x21000       # 132 KiB
slot count             = 1024
window size            = 0x21000 * 1024 = 0x08400000 = 132 MiB
KERNEL_STACK_TOP       = 0xfffffffffffef000
KERNEL_STACK_BOTTOM    = 0xfffffffff7bef000
```

窗口最低地址仍大于 `0xffffff8000000000`，因此 1024 个栈和每个 guard page 都在
40 位 canonical 高半区。编译期断言固定 top、bottom、总大小和 canonical form，避免
以后只移动 top 而忘记完整窗口。

### 5.3 PTE PPN 掩码证明

PALEN=40、页大小 4 KiB 时，合法 PA 位为 `[39:0]`，PTE 中地址部分应为：

```text
PA[39:12]
mask = ((1 << 40) - 1) & ~((1 << 12) - 1)
```

旧式：

```text
((1 << 40) - 1) << 12
```

覆盖的是 `[51:12]`，比硬件 PA 宽 12 位，因而会把不属于 PPN 的高位解释为地址。
当前还断言：

```text
ppn < 1 << (PALEN - 12)
```

使越界物理页不能静默写入 PTE。

### 5.4 PS 编码证明

4 KiB：

```text
page_size = 4096 = 2^12
PS = log2(4096) = 12
```

`PTE_WIDTH_BITS=log2(8)=3` 描述单个 PTE 占 8 字节，只参与页表每级宽度计算，和 TLB
页大小无关。把 3 写进 `STLBPS` 等价于声明 8-byte page；把数值 3 传给“接收字节数”
的 `set_page_size()` 又会得到 `trailing_zeros(3)=0`。修复同时纠正调用单位，并使
refill 分支覆盖 PS 字段而非仅 OR。

## 6. 修复设计

### 6.1 平台能力与地址布局绑定

```rust
board_laqemu: PALEN=48, VALEN=48
board_2k1000: PALEN=40, VALEN=40
```

启动在 `mm::init()` 前读取 CPUCFG1，比对硬件值和构建常量；同时令
`RVACFG.RBits=0`，避免固件遗留 reduced-VA 状态悄悄改变有效地址宽度。

### 6.2 保留 guarded VM stack，不退回 heap 栈

本次只迁移 2K1000 的固定 VA 窗口，继续使用 128 KiB 映射栈和向下增长方向的 4 KiB
未映射 guard。这样既修复地址合法性，也保留“溢出立即 fault”的诊断能力。

回退到 heap `Vec<u8>` 虽可能绕过本次高 VA，但会恢复旧的静默堆破坏风险，不是根因
修复。扩大栈大小同样不能使非 canonical 地址合法。

### 6.3 地址派生类型分开

修复引入并分别使用：

```text
VA_MASK / SEG_MASK       # 完整虚拟地址
VPN_MASK / VPN_SEG_MASK  # 去掉12位页偏移后的页号
VPPN_MASK                # 再去掉偶奇页选择位
PPN mask                 # 受PALEN约束的物理地址部分
```

相似的“高位掩码”不能共享同一个常量，因为每右移一层，字段宽度和符号位位置都变化。

### 6.4 刷新策略与地址空间身份一致

用户页修改走带当前 ASID 的页级 invalidation；内核动态 PGDH 映射可能留下非 global
条目，当前实现采用保守全局刷新。全局 flush 在这条 kernel 映射路径是当前正确性
方案，但不应被推广为“所有用户 PTE 问题一律 full flush”。页级 flush 失效时可用
full flush 做定位对照，最终仍需修正 ASID/global/VPN 操作数。

## 7. 未采用的 workaround

| workaround | 不采用原因 |
|------------|------------|
| 把首个栈临时放回低地址 | 只绕开固定地址，不能证明 1024-slot 窗口和高地址映射正确 |
| 退回 heap `Vec<u8>` 栈 | 失去 guard，栈溢出会静默破坏堆 |
| 只增大内核栈 | 容量与 canonical form 无关 |
| 只看 `mapped_frame()` 成功 | 软件页表不能验证 CPU 地址规范性 |
| 遇到异常一律全 TLB flush | 会掩盖 ASID/VPPN 错误并引入性能退化 |
| 把 PCI 高 PA 直接恒等映射为 VA | 对 40 位页模式可能非 canonical，缓存属性也错误 |
| 把 DMW2 别名写给 DMA | 设备消费总线 PA，不理解 CPU DMW 虚拟别名 |

## 8. 证据矩阵

| 证据 | commit/路径/数值 | 结论 |
|------|------------------|------|
| 原始异常 | `docs/09_debug/bug-la64-kernel-stack-overflow.md` | 保留 `kstack:01`、`AddressError` 与 bad addr 原文 |
| 硬件位宽 | Work_Log：`CPUCFG1=0x03e2727e` | 两字段均解码为 40 |
| 故障边界 | Work_Log：`sched:01` 有、`user:01` 无 | TCB/入队已过，故障在首次高栈恢复到 trap_return 之间 |
| 修复前常量 | `b5826a65^:config.rs` | `PALEN=VALEN=48`，栈 top=`MMAP_BASE-PAGE_SIZE` |
| 修复前 PTE | `b5826a65^:laflex.rs` | PPN 掩码多延伸 12 位 |
| 修复前 restore | `b5826a65^:trap.S` | ASIDBITS OR 入 ASID；PGDL 相同跳过更新 |
| 修复提交 | `b5826a65` | 40 位窗口与完整地址/TLB 审计进入 Git 历史 |
| 当前栈断言 | `config.rs` | top/bottom/window/canonical 由 const assert 固定 |
| 汇编证据 | `b5826a65` Work_Log | objdump 确认 refill PS 和 PGDL/ASID 指令序列 |
| 模拟器 | `b5826a65` Work_Log | QEMU 48/48 进入 stage-1/initproc |
| 实板闭环 | `4705b28d:docs/Work_Log.md` | 40 位高栈、首次切换、PLV3 initproc 全部 PASS |

上述源文件在本文最终审计时相对当时 HEAD 无工作区修改；协作期间 HEAD 曾因同一
ext4 修复提交被重写，所以当前 HEAD 哈希不是本问题的稳定证据。本文只用
`b5826a65` 与 `4705b28d` 锚定 2026-07-10 的修复和最终实板闭环，其他工作区改动不
用于证明该根因。

## 9. 验证矩阵

| 验证层 | 预期 | 结果/证据 |
|--------|------|-----------|
| 静态地址计算 | 40 位栈全窗口 canonical | PASS，const assert + 数值复算 |
| CPU 能力 | 硬件/构建 VALEN、PALEN 一致 | PASS，QEMU 48/48；实板 40/40 |
| 栈探针 | `kstack:01 -> kstack:02` | 最终实板 PASS 记录 |
| 首次调度 | `sched:01` 后进入 `user:01` | 最终实板 PASS 记录 |
| TLB PS | 4 KiB 编码为 12 | 源码 + 目标文件反汇编记录 |
| PGDL/ASID | 两字段分别比较、连续写入 | 源码 + 反汇编记录 |
| 双架构编译 | RV64 后 LA64 | `b5826a65` Work_Log PASS |
| LA64 QEMU | stage-1/initproc | PASS |
| 2K1000 uImage | Load/Entry `0x90000000` | PASS |
| 实板端到端 | `bootm -> initproc` 无 panic | `4705b28d` Work_Log PASS |

本轮只创建复盘文档，未重新执行构建/QEMU/实板；表中结果均按提交时的证据表述。

## 10. 失败边界与剩余风险

- 1024 个内核栈 slot 耗尽仍可能 panic；后续虽修过 zombie queue 漏清，但分配接口
  本身仍应最终改为 fallible。
- 单页 guard 能捕获逐步向下越界，不能保证拦截一次跨过 4 KiB guard 的巨大 SP 跳变。
- 临时 ELF 映射失败路径缺完整 RAII 回滚；`KERNEL_PROGRAM_END` 只解决地址别名上界。
- kernel PT 当前采用全局 TLB flush 保正确性，未来优化必须先证明 global/ASID 语义。
- 本文没有声称表中每个审计缺陷都在实板单独复现；唯一有直接异常因果证据的是
  non-canonical stack。
- 当前支持单核。若启用多核，PTE 修改后的 TLB shootdown 不能只刷新当前核。

## 11. 可复用排障流程

```text
高地址第一次访存失败
-> 先按异常类别分流
   AddressError: CPUCFG/VALEN/canonical/DMW
   PageInvalid或refill: PGD/PTE/VPPN/PS/ASID/TLB
-> 在仍可靠的启动栈上做一次目标地址volatile探针
-> 同时记录VA、PC、SP、PGDH、软件PPN和ASID
-> 用硬件CPUCFG值复算canonical边界
-> 地址合法后再审计VPN/VPPN/PPN字段宽度
-> objdump核对裸汇编CSR写入顺序
-> QEMU验证平台分支，最后以真实硬件进入用户态为门禁
```

## 12. 闭合证据链

```text
TCB和init ELF创建完成，最后日志停在sched:01
-> TaskContext的resume PC/SP与trap_return预期一致
-> 启动栈上的首个高栈volatile探针立即AddressError
-> bad addr=0xffffff7fffffeff8
-> CPUCFG1=0x03e2727e解出VALEN=40
-> 该地址bit39=0但bits63:40全1，硬件页表查询前必然拒绝
-> 栈窗口迁到0xfffffffff7bef000..0xfffffffffffef000
-> 全窗口数学证明canonical，并用const assert固化
-> 同步修正PS/PPN/VPPN/ASID/别名/DMW地址契约
-> QEMU与目标文件反汇编通过
-> 后续实板bootm越过kstack/sched/user探针进入PLV3 initproc
```

该闭环解释了“软件页表看起来正确却仍 AddressError”的矛盾，也说明为何修复不能止于
移动一个栈常量：上板时改变的是整个硬件地址契约。
