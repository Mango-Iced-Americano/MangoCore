---
title: "CPython 实机状态损坏：LSX/FPR 物理别名与上下文恢复"
category: debug
status: resolved
author: MangoCore Team
date: 2026-07-15
last_update: 2026-07-15
tags: [loongarch64, 2k1000la, lsx, fpu, trap, signal, context-switch, cpython]
code_paths:
  - "os/src/hal/arch/loongarch64/mod.rs"
  - "os/src/hal/arch/loongarch64/trap/context.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "os/src/task/signal/frame.rs"
  - "os/src/task/signal/mod.rs"
  - "os/src/syscall/process/signal.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260710/14-loongarch-hwcap-publication.md"
  - "docs/08_testing/cpython-isolated.md"
  - "docs/05_process/signal.md"
entry_points:
  - "bootstrap_init"
  - "__alltraps"
  - "__restore"
  - "sys_sigreturn"
---

# CPython 实机状态损坏：LSX/FPR 物理别名与上下文恢复

## 1. 一句话结论

Alpine LoongArch64 CPython runtime 的基线 libc 本身会执行 LSX，不能靠隐藏 HWCAP
绕开；而真正启用 LSX 后，2K1000LA 又暴露了 `$fN` 与 `$vrN` 低 64 位是同一物理
寄存器这一事实。trap return 若先恢复 128-bit LSX、再恢复 scalar FPR，会在实机破坏
向量高 lane。最终规则是：`EUEN.SXE=1` 时只恢复完整 LSX，关闭时只恢复 FPR；
`sigreturn` 则先把可由 handler 修改的 scalar FPR 合并进 LSX 低 lane，再只做一次完整
LSX 恢复。

## 2. 问题卡

| 项目 | 结论 |
|------|------|
| 前置状态 | iozone 阶段为避免误报，LSX HWCAP/EUEN 暂时关闭 |
| 首个 CPython 故障 | LA64 QEMU 在 musl `memset` 的 `vreplgr2vr.w` 报 `InstructionNonDefined` |
| 为什么不能继续隐藏 | Alpine runtime 的基线代码直接含 LSX，不依赖 IFUNC/HWCAP 才发出 |
| 首轮功能补齐 | CPUCFG2 探测、`EUEN.SXE`、HWCAP_LSX、32×128-bit trap/signal context |
| QEMU 结果 | RV64/LA64 CPython 均 72/72，表面已通过 |
| 实板二次故障 | CPython 首次动态启动可稳定出现状态损坏，未必报非法指令 |
| 深层根因 | scalar FPR 是同编号 LSX vector 的低 64-bit 物理别名；顺序双恢复不独立 |
| 根因修复 | trap return 二选一恢复；signal scalar 低 lane 合并后以 LSX 为唯一恢复源 |
| 主要提交 | `6b6282401f23455ee87867fcb1597abb7af26d64` |
| 最终实板 | 最小命令、20 次启动、signal/thread/subprocess、完整 72/72 均通过 |

## 3. 先区分两个不同阶段

### 3.1 阶段 A：指令根本不能执行

HWCAP 误报修复后，内核有意不启用 LSX，因为当时 trap context 只保存标量 FPU。
这一策略能让不强制 LSX 的 glibc iozone 安全运行，但新的 Alpine CPython runtime
并不满足该前提：其 musl `memset` 基线代码直接包含 `vreplgr2vr.w`。

第一次 LA64 QEMU 启动 CPython 时：

```text
musl memset
  -> vreplgr2vr.w
  -> EUEN.SXE == 0
  -> InstructionNonDefined
```

这与上一篇的 LASX HWCAP 误报不同。上一篇可以通过不发布未支持能力使 loader 选择
普通路径；这里目标二进制已经把 LSX 指令编进基础路径，隐藏 HWCAP 不会重写二进制。

### 3.2 阶段 B：指令可执行，但状态跨 trap 后损坏

补齐 `EUEN.SXE` 和保存区后，QEMU CPython 门禁全部通过；上实板却在首次动态启动
出现可重复的状态损坏。此时已不再是“非法指令”：CPU 能执行 LSX，错误发生在定时器、
syscall、signal 或任务切换造成的保存/恢复边界。

这两个阶段必须分别收口：

| 阶段 | 失败性质 | 必要修复 |
|------|----------|----------|
| A | execution permission/capability 缺失 | 探测硬件，启用 SXE，保存完整 LSX，发布 HWCAP |
| B | 扩展状态恢复语义错误 | 建模 FPR/LSX 物理别名，禁止顺序双恢复 |

## 4. 底层原理：两个名字指向同一寄存器低 lane

### 4.1 不是两套互相独立的寄存器文件

对每个编号 `N`，可以把架构关系画成：

```text
LSX $vrN (128 bit)
+-------------------------------+-------------------------------+
| high 64 bit                   | low 64 bit                    |
+-------------------------------+-------------------------------+
                                  ^
                                  |
                                  +---- scalar FPU $fN (64 bit)
```

`$fN` 与 `$vrN[63:0]` 是别名，不是两个需要各自恢复的独立状态。保存时读取二者没有
副作用，可以同时获得 scalar ABI snapshot 和完整 vector snapshot；恢复时对二者都写，
后一次写会重新定义同一个物理寄存器的部分状态。

### 4.2 错误的“最保险做法”反而破坏状态

直觉上容易写成：

```text
for n in 0..32: VLD   vr[n], lsx_snapshot[n]   # restore 128 bits
for n in 0..32: FLD.D f[n],  fp_snapshot[n]    # restore scalar 64 bits
```

第二步不只是把相同 low 64 bit 再写一遍。实机上 scalar 写会使同一 vector 的其余状态
不再保有第一步刚写入的高 64 bit（高 lane 可被破坏/成为未定义状态）。于是下一次 LSX
运算读到错误向量，错误可传播到 libc 字符串、allocator 或解释器内部数据。

首版 LSX 调试实现曾采用这种顺序双恢复；它是在同一开发轮次中被实板否定的中间态，
没有形成独立 Git 提交或保留原始串口文件。可审计的最终提交 `6b628240` 已是修正后的
二选一路径；中间态及实机症状来自同期 Work_Log 追溯记录。

### 4.3 正确恢复必须有唯一权威快照

trap return 读取 `EUEN.SXE`：

```text
if EUEN.SXE != 0:
    restore vr0..vr31 from complete 128-bit LSX snapshot
else:
    restore f0..f31 from scalar FPR snapshot
```

在 LSX 开启时，完整 vector snapshot 同时包含 low 64 bit，所以无需再写 `$fN`；在
LSX 关闭时，LSX 区不是有效执行状态，恢复 scalar FPR 即可。

## 5. 调试追溯过程

### 5.1 从 QEMU 非法指令反汇编到 baseline LSX

首次 LA64 QEMU 的异常落在 Alpine musl `memset`，反汇编确认指令为
`vreplgr2vr.w`。这个位置很关键：它不是 glibc IFUNC 根据错误 HWCAP 选出的可选优化，
而是该 runtime 可执行基线的一部分。

由此排除两个方向：

- 继续对用户态隐藏 LSX：二进制仍会执行该指令；
- 捕获非法指令并逐条软件模拟：无法建立可维护的完整 LSX ABI，也绕开不了上下文保存。

正确工作包必须一次补齐探测、执行许可、HWCAP、trap context 和 signal context。

### 5.2 第一次实现完整 LSX context

提交 `6b628240` 增加：

- `CPUCFG2_LSX = 1 << 6` 探测；
- 硬件有 LSX 时设置 `EUEN.SXE`；
- 在 HWCAP 中发布 LoongArch `HWCAP_LSX = 1 << 4`；
- `LsxRegs { v: [[u64; 2]; 32] }`，`repr(C, align(16))`；
- `TrapContext` 末尾追加 512 字节 LSX 快照；
- trap entry 的 32 次 `vst` 和 trap return 的 `vld`；
- signal frame 的 LSX 扩展状态及编译期 offset。

`LSX_START = 70 * 8` 与 Rust `offset_of!(TrapContext, lsx)` 之间有 compile-time assert，
避免汇编硬编码 offset 与 Rust layout 静默漂移。

### 5.3 QEMU 全绿后，实板反而暴露第二根因

RV64/LA64 QEMU 的 L3-L9 都得到 72/72，其中包含 signal、线程和 subprocess。若只看
模拟器，这一轮已可误判为完成。但 2K1000LA 首次动态启动稳定损坏，说明：

```text
code/data/runtime image OK enough to reach loader
LSX instruction permission OK (no longer InstructionNonDefined)
QEMU functional tests PASS
real hardware state corrupts across execution boundary
```

调查焦点因而从 HWCAP/非法指令转到 trap restore。对照 LoongArch 寄存器别名语义后，
确认顺序 `VLD` + `FLD.D` 把同一物理状态写了两次。QEMU 没有可靠暴露这种低 lane
别名副作用，所以模拟器 PASS 不能作为该问题的最终门禁。

### 5.4 让恢复分支不再破坏用户通用寄存器

二选一恢复需要临时读取 `CSR_EUEN` 并用 `$t0` 分支。如果先恢复用户通用寄存器，
再用 `$t0` 做选择，就会把刚恢复的用户 `$t0` 覆盖掉。最终 `__restore` 的顺序是：

```text
restore FCSR/FCC/PRMD/ERA
  -> read EUEN into temporary register
  -> restore LSX or scalar FPR
  -> only then restore all user GPRs
  -> restore user SP
  -> ertn
```

这一步不是风格调整，而是二选一路径引入的第二个寄存器活性约束。

## 6. trap 保存为何可以保留两份快照

trap entry 先执行 32 次 `FST.D` 保存 scalar FPR，再在 `SXE` 开启时执行 32 次 `VST`
保存完整 LSX。两者都是读取寄存器并写内存，不会像 restore 那样改写别名寄存器：

```text
fp.f[n]      = vr[n].low64
lsx.v[n][0]  = vr[n].low64
lsx.v[n][1]  = vr[n].high64
```

保留 scalar snapshot 有两个理由：

1. 现有 LoongArch signal `mcontext` 已暴露 scalar FPR 视图；
2. `SXE` 关闭时，trap return 仍需恢复标量程序。

关键不变量不是“内存里只能存一份”，而是“写回物理寄存器时只能有一个权威来源”。

## 7. signal frame 的冲突与合并规则

### 7.1 signal handler 可以修改 scalar mcontext

signal frame 同时包含：

```text
UserContext.mcontext.fp   # 既有 scalar FPU ABI view
UserContext.lsx           # MangoCore 保存的完整 LSX snapshot
```

用户 handler 可能合法修改 scalar mcontext。如果 `sigreturn` 无条件以保存的 LSX
覆盖寄存器，这些 scalar 修改会丢失；若恢复 LSX 后再恢复 FPR，又重现物理别名破坏。

### 7.2 明确 scalar 低 lane 优先

最终 `sys_sigreturn()` 先读取两份用户快照，再合并：

```rust
trap_cx.lsx = restored_lsx;
for (vector, scalar) in trap_cx.lsx.v.iter_mut().zip(trap_cx.fp.f.iter()) {
    vector[0] = *scalar as u64;
}
```

合并后的 `trap_cx.lsx` 是唯一权威状态：

```text
low64  <- handler-visible scalar mcontext
high64 <- saved/handler-visible LSX extension
__restore -> one full LSX load
```

这样既尊重既有 scalar ABI 修改，又不执行危险的二次 scalar restore。

### 7.3 offset 必须来自 layout，而不是手算

`UserContext` 中 signal mask 有 padding，新增 16-byte aligned LSX 字段后，手工
`sizeof` 链很容易错。实现使用：

```rust
UserContext::MCONTEXT_OFFSET = offset_of!(Self, mcontext)
UserContext::LSX_OFFSET      = offset_of!(Self, lsx)
```

`sys_sigreturn` 据此从用户栈读回，坏地址统一 SIGSEGV，避免把 layout 漂移伪装成
“随机向量损坏”。

## 8. 证据链

### 8.1 阶段 A：LSX 必须启用

| 证据 | 推论 |
|------|------|
| LA64 QEMU 在 musl `memset` 的 `vreplgr2vr.w` 报非法指令 | runtime baseline 直接需要 LSX |
| 该指令不依赖 glibc LASX resolver | 隐藏 HWCAP 不是可行终态 |
| 增加 SXE/context 后 QEMU 72/72 | execution permission 与基本保存链闭合 |

该阶段现场来自 `docs/Work_Log.md` 2026-07-13 CPython 条目；仓库没有保留首轮非法
指令的独立原始日志。

### 8.2 阶段 B：物理别名是实板根因

| 证据 | 推论 |
|------|------|
| QEMU 全门禁通过，实板首次动态启动稳定损坏 | 不是普通 syscall 缺失；存在硬件模型差异 |
| FPR 与 LSX low lane 架构上别名 | 顺序双恢复不是幂等的两套状态恢复 |
| 改为二选一后最小命令稳定输出 `123` | 恢复策略与症状变化直接相关 |
| 连续 20 次 `import sys,encodings` 均输出 `123` | 高频启动边界不再复现 |
| signal round-trip PASS | signal frame 合并规则可工作 |
| thread/subprocess PASS | 抢占、futex、clone/exec/wait 等上下文边界覆盖 |

实板“修复前状态损坏”与“20 次最小命令”来自 Work_Log 记录，没有独立保留前者 raw
log；最终完整运行则有 `logs/cpython-la64-board.log` 可逐行复核。

### 8.3 最终原始日志证据

`logs/cpython-la64-board.log` 当前保留最终实板分组：

| 行 | 内容 | 证明范围 |
|----|------|----------|
| 1 | `GROUP START cpython-isolated` | 提取的是完整最终分组 |
| 16-29 | L4 启动、版本、import、prefix 全 PASS | 动态启动与 encodings 稳定 |
| 78-79 | `signal handler roundtrip PASS` | sigreturn 不破坏状态 |
| 121-149 | thread 与 subprocess 全 PASS | 多上下文边界正常 |
| 150-165 | DNS/HTTP/HTTPS 全 PASS | 长链运行没有后续静默损坏 |
| 166-168 | GROUP END、exit 0、125 s | 完整终止而非前缀 PASS |

机器判定 `judge_cpython-isolated.py` 得到 `72/72` 记录在 Work_Log；raw log 本身保存
测试逐项结果，但不内嵌 judge 的 JSON 输出。

## 9. 排除的错误方向

### 9.1 永久关闭 LSX/HWCAP

这对可 fallback 的程序有效，却无法运行已经以 LSX 为 baseline 的 Alpine runtime。
目标既然包含该 runtime，就必须实现完整上下文契约。

### 9.2 只打开 `EUEN.SXE`

会消除立即非法指令，却让向量寄存器跨 timer/syscall/task switch 静默串线，属于把
fail-fast 变成数据损坏。

### 9.3 先 VLD 再 FLD，认为 low lane 数值相同就安全

数值相同不代表写操作对其余 lane 无副作用。实机别名语义已经否定这种假设。

### 9.4 只跑 QEMU

QEMU 能验证 offset、汇编可执行和大部分 context switch，但该轮实际没有可靠模拟
FPR/LSX low-lane alias 的破坏行为。扩展寄存器最终必须在真实硬件上高频 trap 验证。

### 9.5 signal return 只恢复 LSX，忽略 scalar mcontext

这样会吞掉 handler 对现有 scalar ABI 的合法修改。必须先定义冲突优先级，再合并成
唯一快照。

## 10. 修复后的不变量

1. `HWCAP_LSX`、`EUEN.SXE`、trap save/restore 和 signal save/restore 同时启用。
2. `TrapContext::lsx` 固定 16-byte 对齐，汇编 offset 有编译期断言。
3. trap entry 可保存 scalar 与 vector 两个视图；trap return 只能选择一个写回路径。
4. `SXE=1` 时完整 LSX 是权威状态，禁止随后 `FLD.D`。
5. `SXE=0` 时只恢复 scalar FPR，不读取无效 LSX 状态。
6. signal frame 中 scalar FPR 对 low lane 有明确优先级，`sigreturn` 先合并再恢复。
7. 恢复路径用到的临时 GPR 必须在分支结束后才恢复用户值。
8. LASX/LBT 未有同等级 context 支持前继续关闭且不发布。

## 11. 验证矩阵

| 门禁 | 平台 | 结果 | 关注点 |
|------|------|------|--------|
| CPython L3-L9 | RV64 QEMU | 72/72 PASS | 通用修改无跨架构回归 |
| CPython L3-L9 | LA64 QEMU | 72/72 PASS | LSX 汇编/layout/基本上下文 |
| 最小 `-c` | 2K1000LA | 输出 `123` | 动态启动恢复 |
| 20 次 import | 2K1000LA | 20/20 输出 `123` | 高频可重复性 |
| signal round-trip | 2K1000LA | PASS | frame merge + sigreturn |
| thread/futex | 2K1000LA | PASS | 抢占/任务切换 |
| subprocess/pipe/wait | 2K1000LA | PASS | clone/exec/调度边界 |
| 最终 L3-L9 | 2K1000LA | 72/72，exit 0，125 s | 完整实机闭环 |

## 12. 剩余边界

- LASX 256-bit 状态未进入 `TrapContext`，`EUEN.ASXE` 和 `HWCAP_LASX` 必须继续关闭。
- LBT 状态同样未保存，不发布对应 HWCAP。
- 当前 `UserContext.lsx` 是 MangoCore 为完整恢复追加的 ABI 状态；若追求与 Linux
  LoongArch signal extension 完全兼容，需要按其扩展上下文格式单独审计。
- 单核实板门禁已覆盖频繁 timer/trap，但未来 SMP 还需验证 per-CPU lazy/eager FPU
  ownership；当前实现不能自动推出 SMP 正确。
- 新增任何 vector width 时必须重新审查 FPR/LSX/LASX 的 alias 层级，不能复制这次
  “各自保存、全部恢复”的旧思路。

## 13. 证据索引

| 类型 | 位置 |
|------|------|
| 主要修复提交 | `6b6282401f23455ee87867fcb1597abb7af26d64` |
| CPUCFG/EUEN/HWCAP | `os/src/hal/arch/loongarch64/mod.rs` |
| Rust context layout | `os/src/hal/arch/loongarch64/trap/context.rs` |
| trap save/二选一 restore | `os/src/hal/arch/loongarch64/trap/trap.S` |
| signal frame 构造 | `os/src/task/signal/frame.rs`、`os/src/task/signal/mod.rs` |
| scalar-low-lane merge | `os/src/syscall/process/signal.rs::sys_sigreturn` |
| 最终实板 raw log | `logs/cpython-la64-board.log` |
| 阶段性现场和 judge | `docs/Work_Log.md` 2026-07-13/14 CPython 条目 |
| HWCAP 前因 | `docs/09_debug/la64_on_board/260710/14-loongarch-hwcap-publication.md` |

## 14. 可复用调试模式

当 SIMD 程序“QEMU 全绿、实机跨 syscall/抢占后随机坏数据”时，不要只检查是否漏存
寄存器，还要检查**不同寄存器名字是否映射同一物理状态**：

```text
instruction legal?
  -> HWCAP and enable bit aligned?
  -> full-width state saved?
  -> aliasing views restored once or multiple times?
  -> signal handler edits have a defined precedence?
  -> real hardware high-frequency trap gate?
```

“保存得越多、恢复得越全”不是普遍正确；有物理别名时，恢复必须选出唯一权威状态。
