---
title: "2K1000LA Python 非对齐访问陷阱风暴根因复盘"
category: debug
status: confirmed
author: MangoCore Team
last_update: 2026-07-17
tags: [loongarch64, 2k1000la, python, unaligned, trap, uaccess, cow, tlb, performance]
code_paths:
  - "os/src/hal/arch/loongarch64/trap/mod.rs"
  - "os/src/hal/arch/loongarch64/trap/trap.S"
  - "os/src/mm/uaccess.rs"
  - "os/src/mm/page_fault.rs"
  - "os/src/mm/vma.rs"
  - "os/src/hal/arch/loongarch64/laflex.rs"
related_docs:
  - "docs/09_debug/la64_on_board/260717/01-python-performance-baseline.md"
  - "docs/09_debug/la64_on_board/260717/05-strict-align-first-experiment.md"
  - "docs/09_debug/perf_diag.md"
---

# 2K1000LA Python 非对齐访问陷阱风暴根因复盘

## 0. 一句话结论

2K1000LA 实板报告 `CPUCFG1=0x03e2727e`，其中 UAL capability bit 为 0，不能像当前
LA64 QEMU 那样直接完成普通非对齐访存。旧 CPython runtime 在 Python 对象、字符串和
浮点 workload 中生成了大量非对齐整数 load/store；每条指令进入完整用户 trap，当前
handler 又把一个 2/4/8-byte 访问拆成逐字节通用 uaccess，使一次本应很小的用户态
访问反复执行 VM 获取、锁、fault/COW 分类、PTE 权限修改和 TLB invalidate。

`bm_float` 匹配诊断窗口内发生 `3,000,039` 次 trap，Rust handler ticks 对应
`47.679 s`，占 `50.070 s` system time 的 `95.22%`。这不是“Python 调了很多 syscall”，
而是用户指令被内核模拟后计入了 system time。

## 1. 问题卡

| 属性 | 内容 |
|------|------|
| 表面现象 | 纯对象/数值 workload 没有显式 I/O，却有 30%–70% sys；QEMU 明显更快 |
| 首要硬件事实 | 实板 `CPUCFG1=0x03e2727e`，UAL=0 |
| 最小正对照 | `bm_float` workload body，3,000,039 次 trap |
| 近匹配正对照 | `bm_string` workload body，373,371 次 trap |
| 负对照 | `bm_nbody` body 只有 39 次 trap，sys 约 0.026 s |
| 直接放大器 | trap 内对访问宽度逐 byte 调用 `copy_from_user`/`copy_to_user` |
| 二级放大器 | private store 的 fault/COW 权限恢复和逐 byte 单页 TLB invalidate |
| 证据状态 | trap 风暴和当前 handler 放大已确认；具体 cache 冲突未确认 |
| 第一次处理 | 用户态完整依赖闭包 `-mstrict-align`，内核 handler 不改 |

## 2. 为什么先查 trap，而不是 syscall 分布

production 基线中，`regex/string/float/chaos/dict/list` 等纯计算或对象操作出现异常高
sys。只看 `read/write/openat/mmap` syscall 计数无法解释这些 workload，因为主要工作
发生在解释器循环和内存对象上。

关键区分是：rusage 的 system time 统计 CPU 处于内核态的时间，不只统计显式
syscall。用户执行 AddressNotAligned 指令后进入异常处理，同样累计为 sys。诊断因此
采用以下窗口：

```text
解释器启动和 import
    -> 不计入
重置 profile/core counter
    -> 开启 stats
只运行 benchmark body
    -> 关闭 stats
读取 trap 宽度、handler ticks、TLB 和 rusage
```

这避免把动态 loader、suite import 或 JSON 输出本身的非对齐访问错误归给算法。

## 3. 实板定量证据

### 3.1 三个 workload 对照

| body | elapsed/s | user/s | sys/s | traps | handler ticks | handler/s | handler/sys |
|------|----------:|-------:|------:|------:|--------------:|----------:|------------:|
| string | 11.457334 | 2.836119 | 8.602696 | 373,371 | 783,003,473 | 7.830035 | 91.02% |
| float（deepdiag） | 71.117 左右 | 21.9 左右 | 49.175 左右 | 3,000,039 | 4,680,636,915 | 46.806369 | 95.18% |
| float（相邻 diag） | 72.080175 | 21.895004 | 50.070068 | 3,000,039 | 4,767,941,219 | 47.679412 | 95.22% |
| nbody | 8.539356 | 8.503722 | 0.025818 | 39 | 62,820 | 0.000628 | 2.43% |

Rust handler ticks 不包含 trap 汇编入口和出口保存/恢复寄存器的时间，因此
`handler/sys` 是保守下界。即便如此，float/string 已能解释绝大多数 sys。

### 3.2 指令宽度说明热点不是浮点异常分支

| workload | load2 | load4 | load8 | store2 | store4 | store8 | float load/store |
|----------|------:|------:|------:|-------:|-------:|-------:|-----------------:|
| string | 9 | 2,183 | 15,579 | 115,263 | 102,327 | 138,010 | 0 |
| float | 9 | 1,800,008 | 1,200,003 | 3 | 11 | 5 | 0 |

`bm_float` 是浮点 workload，不代表故障指令是浮点 load/store。本次计数显示热点全部在
整数访问；约每轮出现 3 次 4-byte load 和 2 次 8-byte load。不能因为 benchmark 名称
含 float 就去优化 `fld/fst` 分支或 libm 单个函数。

### 3.3 string 的逐字节写与 TLB 一一对应

string 的非对齐 store 展开字节数为：

```text
115,263 × 2 + 102,327 × 4 + 138,010 × 8
= 1,743,914 byte stores
```

相同 workload 的 memory profile 窗口记录单页 TLB invalidate `1,761,177` 次。两者
之比为 `1,743,914 / 1,761,177 = 99.02%`。如果 handler 只是软件拼出一个数值，TLB
次数不应与访问宽度展开后的 byte 数近似一一对应。这个闭合把放大位置进一步定位到
逐 byte store 之后的 PTE/COW 权限恢复路径。

## 4. 源码执行链

### 4.1 通用 trap 入口成本

`trap/trap.S` 的用户 trap 入口保存通用寄存器、标量 FPR，并在启用 LSX 时保存向量
状态；返回时执行对应恢复。无论故障只是一条 4-byte load 还是 8-byte store，都要先
支付整套入口/出口成本。这部分不在 Rust handler 自己的计时范围内。

### 4.2 解码后按字节模拟

`trap/mod.rs` 对 AddressNotAligned 读取故障指令、解码 opcode/寄存器/宽度，再进入
`read_unaligned_user` 或 `write_unaligned_user`。当前 helper 对 `0..width` 循环，每个
byte 分别调用通用 `copy_from_user` 或 `copy_to_user`。

一个 8-byte store 因此不是“一次页解析后写 8 字节”，而是 8 次完整用户复制入口。

### 4.3 每个 byte 重取 VM、加锁和 fault-in

`mm/uaccess.rs` 的通用 copy path 每次重新取得当前地址空间、获取 VM 锁并调用
`fault_in_user_va()`。这个设计对普通长 buffer copy 可由外层分块摊销，但在非对齐
handler 里被以单字节粒度调用，锁和页检查成为访问宽度的倍数。

### 4.4 private store 进入 COW/权限恢复

`page_fault.rs` 和 `vma.rs` 对 private writable mapping 的 store 进行 present、权限、
COW 和 PTE flag 分类。即使物理页已经存在，逐 byte helper 仍可能反复经过权限恢复
逻辑。PTE flags 修改后，`laflex.rs` 执行单页 TLB invalidate。string 的 99.02% 对应
关系证明这不是仅存在于源码的潜在路径，而是实板热点。

完整链为：

```text
用户非对齐 load/store
  -> trap.S 保存上下文
  -> trap/mod.rs 取指和解码
  -> for byte in access_width
       -> copy_{from,to}_user
       -> 取当前 VM + VM lock
       -> fault_in_user_va
       -> private store / COW / PTE permission
       -> 单页 TLB invalidate
  -> 写回目标寄存器、推进 PC
  -> trap.S 恢复上下文
```

## 5. 诊断构建为什么不能直接替代 production 时间

相邻 production 和 `perf_diag stats_on=0` 使用同一 HEAD、normal feature、initramfs、
suite、runtime 和 ext4 路径，但 diagnostic feature 改变了 ELF 布局：

| workload | production/s | diag off/s | 差异 | 关键拆分 |
|----------|-------------:|-----------:|-----:|----------|
| nbody | 8.651655 | 8.547137 | -1.21% | user/sys 都稳定 |
| string | 15.834437 | 11.305700 | -28.60% | sys 8.562/8.469 s 稳定，user 变化 |
| float | 149.893382 | 72.492407 | -51.64% | sys 50.116/50.187 s 稳定，user 变化 |

同一 diagnostic ELF 内，float `stats_on=0/1` 为 `72.492/72.080 s`，差 `-0.57%`；
string/fileio 也低于 1.3%。这证明运行时计数开关税很低，却不能证明 feature 对代码
布局无影响。

当前高概率解释是数百万次用户/内核切换使用户和内核代码布局、I-cache/BTB 等非常
敏感；没有 PMU cache-miss 数据，不能写成已确认的 L1/L2 冲突。因此：

- production 决定正式绝对性能；
- diagnostic 只解释 trap 数、宽度、handler 时间和 TLB 等机制；
- 不用 diagnostic 的 72 s 覆盖 production float 的 150 s 基线。

## 6. 被排除或尚未证明的解释

| 假设 | 判定 | 证据 |
|------|------|------|
| 主要是普通 syscall 太多 | 排除为 float/string 主因 | handler 已解释 91%–95% sys |
| 主要是 libm 单个浮点函数 | 排除 | trap 分类全是整数 load/store；micro probe 不支持单函数归因 |
| 所有 Python workload 都同样受影响 | 排除 | nbody body 只有 39 次 trap |
| `perf_diag` 计数器本身造成 50 s | 排除 | 同 ELF stats on/off 低于 1.3% |
| production/diag 差异就是探针税 | 排除 | stats off 已存在巨大选择性 user 差异 |
| 已经知道 fault PC Top-N | 未测 | 用户决定本轮不采 PC Top-N |

## 7. 正确性风险与本轮处理边界

当前 handler 的未知 opcode 和部分 user copy 失败路径仍含 `unwrap()`/panic 风险；不支持
的原子/向量语义也不能靠普通逐字节 load/store 安全模拟。FP 与 LSX 共享物理低 lane，
未来处理非对齐浮点指令时还必须保证 trap context 的权威状态一致。

这些属于内核兼容 handler 的后续设计，本轮明确不改。第一次实验只重编用户态完整
runtime，使正常 Python 路径不再产生此类指令。结果与证据等级见
[05-strict-align-first-experiment.md](05-strict-align-first-experiment.md)。

## 8. 闭合证据

- 原始 string：[`ext4_string_core_class-1-d6cf6951.log`](raw-data/20260716T-cpython-deepdiag/raw/ext4_string_core_class-1-d6cf6951.log)
- 原始 float：[`adjacent_diag_float_core-1-530741d6.log`](raw-data/20260716T-perf-diag-structural-ab-run/raw/adjacent_diag_float_core-1-530741d6.log)
- 原始 nbody：[`nbody_body_core-1-811202c0.log`](raw-data/20260716T-cpython-deepdiag/raw/nbody_body_core-1-811202c0.log)
- 相邻结构 A/B：[`structural_ab.csv`](raw-data/20260716T-perf-diag-structural-ab-run/reports/structural_ab.csv)
- 全部 counter：[`counter_deltas.csv`](raw-data/20260716T-cpython-deepdiag/reports/counter_deltas.csv)

至此已形成“硬件不支持 → 用户指令触发 → handler 次数/宽度 → handler/sys 对齐 →
逐 byte store/TLB 对齐 → 源码链”的闭环。
