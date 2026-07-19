---
title: "glibc 启动即非法指令：跨架构 AT_HWCAP 误报"
category: debug
status: resolved
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, elf, auxv, hwcap, glibc, ifunc, lasx]
code_paths:
  - "os/src/mm/address_space.rs"
  - "os/src/hal/mod.rs"
  - "os/src/hal/arch/mod.rs"
  - "os/src/hal/arch/riscv/mod.rs"
  - "os/src/hal/arch/loongarch64/mod.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/14a-loongarch-lsx-fpr-physical-alias.md"
  - "docs/03_fs/2k1000-full-test-disk.md"
  - "docs/01_architecture/hal-and-platform.md"
entry_points:
  - "AddressSpace::create_elf_tables"
  - "hal::user_hwcap"
  - "loongarch64::user_hwcap"
---

# glibc 启动即非法指令：跨架构 `AT_HWCAP` 误报

## 1. 一句话结论

LA64 glibc iozone 不是死在文件 I/O，而是在进入 workload 前由动态加载器根据错误的
`AT_HWCAP=0x112d` 选择了 LASX resolver；`0x112d` 在 RISC-V 表示 IMAFDC，放到
LoongArch 位命名空间却同时声称 LASX/LBT 等能力。修复是把 HWCAP 下沉到 HAL：
RISC-V 保留 `0x112d`，LoongArch 只发布“CPU 支持、内核已启用、trap/signal 能完整
保存”三者交集，而不是为了让测试继续跑而单独屏蔽 glibc。

> 本文只分析 HWCAP 误报。LSX/FPR 物理别名导致的上下文损坏是另一个根因，见
> [14a-loongarch-lsx-fpr-physical-alias.md](14a-loongarch-lsx-fpr-physical-alias.md)。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 触发 | 2K1000LA 运行 glibc 动态链接版 iozone |
| 稳定故障点 | `_dl_runtime_resolve_lasx`，PC `0xf60010dfc` |
| 异常 | `InstructionNonDefined`；现场反汇编为 LASX `xvst` 路径 |
| 对照 | musl 版完整运行 1331 s、退出 0 |
| 第一误导 | 当时正在迁移 iozone 到 SSD scratch，表面像文件系统/镜像问题 |
| 直接根因 | 所有架构共用硬编码 `AT_HWCAP=0x112d` |
| 深层根因 | 把架构私有 ELF ABI bitmask 当成跨架构通用能力集合 |
| 根因修复 | `AddressSpace` 调 HAL；LA64 从 CPUCFG 和内核保存能力生成 HWCAP |
| 修复提交 | `e764958a358ff36c91cbcb53cfbb5a937f80eaed` |
| 修复后 | glibc iozone 完整运行 1229 s、GROUP END、退出 0 |

## 3. 底层原理

### 3.1 `AT_HWCAP` 是用户态可执行契约，不是说明文字

内核加载 ELF 时，会在初始用户栈的 auxiliary vector 中写入 `AT_HWCAP`。动态加载器
在应用 `main()` 之前读取它，用于选择：

- IFUNC 实现；
- 动态符号解析 trampoline；
- memcpy/memset 等架构优化；
- 保存额外寄存器的 resolver 入口。

因此错误的一位不是“信息展示不准确”，而是在告诉用户态：**这些指令现在可以安全
执行，并且内核会跨 syscall、抢占、信号和任务切换保存其状态。** 动态加载器依赖
这个承诺发出扩展指令是合理行为。

### 3.2 HWCAP 的位号由架构 ABI 定义

旧代码在通用 `AddressSpace::create_elf_tables` 中直接写：

```rust
// `0x112d` means IMADZifenciC, aka gc
AuxvEntry::new(AuxvType::HWCAP, 0x112d)
```

`0x112d` 展开的置位位号为：

```text
0x112d = bit 0 | bit 2 | bit 3 | bit 5 | bit 8 | bit 12
```

同一个数值在两个 ABI 中完全不是同一个能力集合：

| bit | RISC-V 字母位图 | LoongArch HWCAP 命名空间 |
|-----|------------------|---------------------------|
| 0 | A | CPUCFG |
| 2 | C | UAL |
| 3 | D | FPU |
| 5 | F | LASX |
| 8 | I | CRYPTO |
| 12 | M | LBT_MIPS |

所以对 RISC-V 合理的 `0x112d`，在 LoongArch 上等价于告诉 glibc：“LASX resolver
可用，并且 LBT_MIPS 也可用”。数值相同不代表语义可移植。

### 3.3 硬件有扩展也只是必要条件

对扩展寄存器类能力，安全 HWCAP 应满足：

```text
published_to_userspace
  = hardware_present
  ∩ execution_enabled_by_kernel
  ∩ state_saved_and_restored_by_kernel
  ∩ signal_ABI_supported
```

CPUCFG 只回答第一项。即使硬件实现 LSX/LASX，如果 `EUEN.SXE/ASXE` 没打开，指令
仍会触发异常；即使指令可以执行，如果 trap context 不保存寄存器，程序也会在下一次
抢占后静默数据损坏。故修复不能只是把 CPUCFG 原样翻译成 auxv。

在提交 `e764958a` 时，MangoCore 只有标量 FPU context，因而有意不发布也不启用
LSX、LASX、LBT。后续 LSX context 完成后才发布 LSX；LASX/LBT 至今仍不发布。

### 3.4 为什么首先落在 `_dl_runtime_resolve_lasx`

glibc 的动态链接器根据 HWCAP 选择架构优化 resolver。错误 bit5 使其选中 LASX 版本，
于是第一次需要 lazy binding 时就进入 `_dl_runtime_resolve_lasx`。该路径中的 `xvst`
在当前硬件/内核执行契约下不可用，CPU 报 `InstructionNonDefined`。

这解释了三个关键观测：

1. PC 每次固定在 loader 地址，而不是 iozone 文件读写函数；
2. 故障发生在 workload 之前；
3. musl 版能跑完，glibc 动态版立即失败。

musl PASS 不是“硬件支持 LASX”的证据，只说明这份 musl 启动链没有依据同一错误
HWCAP 走入 glibc 的 LASX resolver。

## 4. 调试追溯过程

### 4.1 先按正在改动的存储路径排查，但症状不符合 I/O 故障

该问题出现在 iozone 从只读测试盘迁移到 P2 `/scratch` 的阶段。自然候选包括：

- 测试二进制复制损坏；
- 动态库路径错误；
- SSD 读错误；
- 可写工作目录创建失败；
- iozone 本身触发未实现 syscall。

但异常类型是 `InstructionNonDefined`，PC 又稳定落在 `_dl_runtime_resolve_lasx`。若是
文件损坏，预期是 ELF 校验/映射错误或不稳定指令流；若是 syscall，PC 应在应用调用点
并有明确 errno。固定 loader 符号把调查层级从文件系统上移到 ELF ABI/CPU 扩展。

### 4.2 用 musl/glibc 差异定位到动态加载器选择

同一板、同一 scratch、同一测试组中：

```text
musl iozone  -> complete, 1331 s, exit 0
glibc iozone -> _dl_runtime_resolve_lasx @ 0xf60010dfc
                InstructionNonDefined
```

这组对照没有证明所有文件系统路径正确，但足以显著降低“块设备/DMA/通用 VFS”作为
直接根因的可能性：相同 I/O 基础设施可以支撑 musl 完整负载，而 glibc 在负载前失败。

### 4.3 反查 ELF 初始栈，找到跨架构硬编码

沿 loader 决策输入回溯到 ELF auxv，发现 `AddressSpace` 对 RV64/LA64 都写 `0x112d`，
旁边注释还明确说明它是 RISC-V ISA 字母位图。这是能闭合根因的源码证据：

```text
generic address-space code
  -> hard-coded RISC-V HWCAP number
  -> LA64 auxv receives same integer
  -> LA64 ABI decodes bit5 as LASX
  -> glibc chooses _dl_runtime_resolve_lasx
  -> LASX instruction is undefined under current execution contract
```

### 4.4 修复后做同负载反证

若根因真是 scratch 或 iozone，单改 auxv 不应使整个 glibc workload 恢复。实际在
HWCAP 架构化后，glibc iozone 完整运行 1229 s，出现 GROUP END 并退出 0。修复变量
与结果之间具有直接机制对应，且不需要修改 iozone/glibc 二进制。

## 5. 修复设计

### 5.1 通用层只负责写 auxv，不解释架构位

```rust
AuxvEntry::new(AuxvType::HWCAP, crate::hal::user_hwcap())
```

`AddressSpace` 不再知道 `0x112d`。这不是简单移动常量：它明确规定 HWCAP 语义属于
HAL/architecture，而不是 ELF 栈布局通用代码。

### 5.2 RISC-V 保持原 ABI

```rust
pub fn user_hwcap() -> usize {
    // IMAFDC, bit position derived from extension letter.
    0x112d
}
```

这保证修复 LA64 不会改变原 RISC-V 用户态能力选择。

### 5.3 LoongArch 逐项构造安全集合

LA64 读取 CPUCFG1/2，按 LoongArch 位定义逐项加入 CPUCFG、UAL、FPU、CRC32、
COMPLEX、CRYPTO、LVZ、PTW、LSPW 等。扩展寄存器能力额外受内核实现约束：

```text
e764958a 时：LSX/LASX/LBT 全部省略，EUEN 对应扩展关闭
6b628240 后：CPUCFG2 有 LSX + 内核保存 LSX -> 发布 HWCAP_LSX，启用 EUEN.SXE
当前：       LASX/LBT context 未实现 -> 对应 HWCAP/EUEN 继续关闭
```

不能通过“CPUCFG 返回 1”直接发布 LASX，因为 HWCAP 还承诺上下文完整性。

## 6. 证据链与证据等级

### 6.1 闭环矩阵

| 环节 | 证据 | 结论 |
|------|------|------|
| 稳定症状 | Work_Log 记录固定 PC `0xf60010dfc`、符号 `_dl_runtime_resolve_lasx` | 失败发生在 loader LASX 路径 |
| ABI 输入 | `e764958a^` 的 `address_space.rs` 硬编码 `0x112d` | LA64 确实收到 RV 位图 |
| 位级解释 | `0x112d` 含 bit5；LA64 bit5 为 LASX | loader 选择有明确输入来源 |
| 实现缺口 | `e764958a` 的 LA HAL 注释/实现省略 LSX/LASX/LBT context | 旧 HWCAP 超出内核承诺 |
| 修复 | `e764958a` 改为 `hal::user_hwcap()` | 删除跨架构污染源 |
| 结果 | glibc 同负载 1229 s、GROUP END、exit 0 | 修改与故障消失一致 |
| 对照回归 | musl 1331 s、RV/LA 构建通过 | 存储路径和另一架构未被破坏 |

### 6.2 原始证据边界

仓库当前没有找到保留 `_dl_runtime_resolve_lasx @ 0xf60010dfc` 的独立原始串口日志；
该现场来自 `docs/Work_Log.md` 2026-07-12 `board/test` 条目和同步架构文档。本文把它
标注为“Work_Log 记录”，不声称是当前可逐行复核的 raw log。

源码前后差异和提交对象仍可直接复核：

```bash
git show e764958a^:os/src/mm/address_space.rs
git show e764958a:os/src/mm/address_space.rs
git show e764958a:os/src/hal/arch/loongarch64/mod.rs
git show e764958a:os/src/hal/arch/riscv/mod.rs
```

## 7. 排除的错误修法

### 7.1 在 glibc/测试脚本中禁用 LASX

这只能绕过一个消费者。其他动态程序仍会读取同一 auxv 并选择非法路径，且内核仍在
对用户态撒谎。根因必须在 HWCAP 生产端修复。

### 7.2 对 LA64 统一返回 0

返回 0 可以避免误报，却会永久压低 CPUCFG/UAL/FPU/CRC32 等已经安全支持的能力，
也掩盖架构契约没有建模。正确做法是逐位求安全交集。

### 7.3 只打开 `EUEN.ASXE`

打开执行权限不能补齐 trap、任务切换和 signal frame 保存。程序可能不再立即非法
指令，却在定时器中断后静默破坏向量状态，故障更难定位。

### 7.4 看到 musl PASS 就判定 CPU 扩展正常

不同 libc 的 IFUNC/loader 路径不同。未执行某扩展指令只能证明“这条路径没触发”，
不能证明扩展、EUEN 或 context save 正确。

## 8. 与 LSX/FPR 别名问题的严格分界

| 问题 | HWCAP 误报（本文） | LSX/FPR 物理别名（14a） |
|------|--------------------|-------------------------|
| 触发 | glibc 根据错误 auxv 选择 LASX resolver | LSX 已合法启用后，trap/signal 恢复状态 |
| 表象 | 立即 `InstructionNonDefined` | 运行一段时间/首次动态启动时数据损坏 |
| 根因层 | ELF ABI 能力发布 | 寄存器物理别名与恢复顺序 |
| 修复 | 架构化并收紧 HWCAP/EUEN | LSX 与 scalar FPR 二选一恢复、sigreturn 合并 |
| QEMU 局限 | 可复现非法指令选择 | 未必精确模拟 FPR/LSX alias |

把两者合并成“LSX 有 bug”会丢失关键因果：本文在最初阶段甚至没有启用 LSX；错误是
声称 LASX 可用。14a 则发生在确实需要启用 LSX 的 CPython runtime 上。

## 9. 修复后的不变量

1. 通用 ELF 层不出现任何架构 HWCAP 数值。
2. 每个架构用自己的 UAPI 位命名空间生成 `AT_HWCAP`。
3. CPUCFG 只构成硬件候选，不自动等价于用户可用。
4. 扩展 HWCAP、EUEN 和 trap/signal context 必须同时演进。
5. LASX/LBT 在完整 context 支持前不得发布，即使某颗 CPU 报告硬件存在。
6. 验收必须包含动态 glibc 路径，静态程序或单一 libc PASS 不足以收口。

## 10. 验证结果

| 门禁 | 结果 | 证据性质 |
|------|------|----------|
| musl iozone 完整负载 | PASS，1331 s，exit 0 | Work_Log 实板记录 |
| glibc 修复前 | FAIL，固定 loader PC，非法指令 | Work_Log 实板记录 |
| glibc 修复后 | PASS，1229 s，GROUP END，exit 0 | Work_Log 实板记录 |
| RV64 kernel build | PASS | Work_Log 记录 |
| LA64 kernel build | PASS | Work_Log 记录 |
| 2K1000 scratch 镜像 build | PASS | Work_Log 记录 |
| LASX/LBT 用户能力 | NOT ADVERTISED | 当前源码可核验 |

## 11. 证据索引

| 类型 | 位置 |
|------|------|
| 修复提交 | `e764958a358ff36c91cbcb53cfbb5a937f80eaed` |
| 旧硬编码 | `e764958a^:os/src/mm/address_space.rs` |
| 当前 auxv 调用 | `os/src/mm/address_space.rs::create_elf_tables` |
| RISC-V 位图 | `os/src/hal/arch/riscv/mod.rs::user_hwcap` |
| LoongArch 能力交集 | `os/src/hal/arch/loongarch64/mod.rs::user_hwcap` |
| 实板追溯记录 | `docs/Work_Log.md` 2026-07-12 `board/test: 迁移 iozone` |
| 后续 LSX 根因 | `docs/09_debug/la64_on_board/260710/14a-loongarch-lsx-fpr-physical-alias.md` |

## 12. 可复用调试模式

当“静态/musl 正常、动态 glibc 在 `main` 前固定地址非法指令”时，优先按以下链路查：

```text
fixed loader symbol
  -> disassemble the exact instruction
  -> identify required ISA extension
  -> decode AT_HWCAP in the target architecture namespace
  -> compare CPUCFG / execution-enable register / context-save support
  -> rerun the same dynamic workload
```

不要先围绕应用 syscall 大面积加日志。固定在动态加载器的扩展 resolver，本身已经是
“能力协商错误”的高信息量指纹。
