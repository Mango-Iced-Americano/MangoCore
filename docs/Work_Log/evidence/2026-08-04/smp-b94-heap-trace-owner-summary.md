# B94 heap_trace 缓冲所有权与 BSS 证据摘要

## 范围与结论

本节点只收口 `heap_trace` 特性下的 MM 诊断状态，不修改追踪算法、
默认 feature-off 路径、FS、Net 或 Driver。原实现由 `TRACE: Mutex<TraceState>`
保护状态，但状态只持有指向两个 `static mut` 数组的裸指针，因此还需要
手写 `unsafe impl Send`。新实现让 `TraceState` 直接拥有数组，只有 mutex
guard 才能派生 `&mut`；`Send` 由字段类型自动推导。

## 安全不变量

- `TraceState` 只含整数和定长数组，自动满足 `Send`；`spin::TicketMutex<T>`
  只在 `T: Send` 时提供共享安全性。
- active/site 的可变访问都要求 `&mut TraceState`，safe indexing 取代了
  raw-pointer arithmetic，probe 取模和 64 次上限未改。
- allocator 分配路径先释放 heap lock 再记录 alloc；dealloc 先记录再取
  heap lock，没有 heap/TRACE 锁嵌套。
- static 的 const 初始化不在运行期构造 25 MiB 栈上临时值。
- 六份 RV64/LA64 linker script 都在 `sbss` 之后收集 `*(.bss .bss.*)`，
  因此 `.bss.heap_trace` 位于 BSP 清零区。

## DeepSeek 协作与人工裁决

DeepSeek max 冻结只读审查未发现阻塞项，并指出必须对真实 ELF 做
NOBITS 检查。验证阶段的审核分为三层：

1. 第一次 RV64 网关在命令展开时因 awk `{print $3}` 被 Python format
   当作占位符而失败，未启动 Docker 构建，不计为内核 RED。
2. 转义后 RV64 真实运行 145.360 s，8 核 SMP 34/34，无 mutation；旧脚本
   正好先命中大 `TRACE`，打印 `0x19a0048 / section 5`。
3. LA64 真实运行 143.659 s，8 核 SMP 34/34，无 mutation；但旧 grep
   先命中 `TRACE_ENABLED`，且脚本没有 `set -e`，所以错误打印
   `size=1 section=2770` 仍返回 0。DeepSeek 将此误解为架构布局差异；GPT
   拒绝这条结论。

最终在同一 Docker 产物上使用精确 `heap_trace5TRACE$`、`set -euo pipefail`
和 `$((trace_size))` 十六进制转换复核：

| 架构 | 大符号尺寸 | 十进制 | section | section type |
|---|---:|---:|---:|---|
| RV64 | `0x19a0048` | 26,869,832 | 5 | `.bss` / `NOBITS` |
| LA64 | `0x19a0048` | 26,869,832 | 2770 | `.bss` / `NOBITS` |

## 冻结信息与验收边界

- baseline HEAD: `37617eb9daec4089fb29c9074ccacad553e49e71`
- tracked diff SHA-256: `c0b1a717fe0ff515f6c45a8ac1f154476118034207c7eae68689ddc0ed892f91`
- RV64 child: `agent-847b79531ef9-r02-rv64-heap-trace-ktest`，PASS，34/34
- LA64 child: `agent-3470b7946347-r01-la64-heap-trace-ktest`，PASS，34/34
- 两个子 runner 均 `process_exit_code=0`、`timed_out=false`、
  `mutation_detected=false`。

本证据证明 feature-on 的双架构类型检查、实际 8 核访问和 BSS 布局；
不宣称 FS/Net/Driver 共享子系统已审计，也不解除普通用户任务的 CPU0
默认 affinity。
