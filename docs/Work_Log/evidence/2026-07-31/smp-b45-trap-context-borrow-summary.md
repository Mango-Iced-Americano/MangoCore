# SMP B45 trap context 借用边界实施证据

状态：`pass`

## 变更边界

- `TaskControlBlockInner` 的 trap context 访问入口由返回
  `&'static mut TrapContext` 改为 `trap_context_mut(&mut self) -> &mut TrapContext`，
  使可变引用生命周期受 `task.inner` guard 约束。
- 删除 `current_trap_cx()`；该 helper 从临时 guard 返回可变引用，调用点看不到真实锁
  生命周期。
- init/clone 路径使用显式短作用域保存 trap context，并在 PTE 修改、TLB ack、共享资源
  登记前释放 `task.inner`。
- LA64 未对齐访存改为锁内快照 PC/store 源寄存器、锁外 copyin/copyout、锁内校验 PC
  后提交 load 结果；没有增加 wrapper、状态机或测试专用生产字段。

## 正确性边界

Rust Reference 把违反引用别名规则、在引用存活期间修改其指向对象列为未定义行为。原接口
把直映区上的可变引用伪装成 `'static`，互斥锁 guard 析构后引用仍可继续使用，因而不能用
“当前任务通常只在一个 CPU 运行”证明安全。新接口让编译器直接拒绝引用越过 guard。

LA64 用户指令/数据复制可能触发缺页和 TLB shootdown，所以不能用一把长
`task.inner` 锁包围整段模拟。重新加锁时的 PC 断言用于暴露违反 current-owner 协议的
写者，避免把过期结果静默覆盖进新的 trap frame。

B45 只收紧借用与一个既有调用链的锁跨度。`sys_sigreturn()` 仍跨用户 frame 读取持有
`task.inner`，作为下一独立节点处理；本证据不宣称已经完成全部 signal uaccess 锁序。

## DeepSeek 只读审查与裁决

冻结设计审查确认原 `&'static mut` 接口不健全，并建议删除 current helper、让引用绑定
inner guard。审查提出复制整个 `TrapContext` 的方案未采纳，因为全量写回会覆盖不属于未
对齐指令的字段；最终只快照 PC/store 源寄存器并提交 load 目标与 PC。审查指出
`sys_sigreturn()` 的长锁问题有效，按独立 B46 记录而未隐藏在 B45 中扩张。

最终验证由 DeepSeek 只读执行四项冻结任务并生成汇总；GPT/Codex 独立核对每个 child
manifest、TAP/judge 总数、失败集合、退出码和源码指纹，没有仅接受模型文本中的 PASS。

## 环境与源码指纹

- 被测 HEAD：`2c1e4689bfc8af96500ef7698bd93c3a31fca83e`
- 最终可执行源码 diff SHA-256：
  `b7ee48be016f5be19725e7eba5be807ea4bb5b2b3e17bd0b5f24935d6a477f6d`
- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2

## 验证结果

首轮 RV64 build 在新生命周期约束下暴露两处 guard 作用域过长和一处不可变 guard：
两个 `E0505`、一个 `E0596`。修正为显式块作用域和可变 guard 后，在最终源码指纹上执行：

| Job | 配置 | 结果 | 用时 |
|-----|------|------|------|
| `smp-b45-rv64-build-r2` | RV64 normal, 8 核 | PASS | 130.497 s |
| `smp-b45-la64-build` | LA64 normal, 8 核 | PASS | 130.621 s |
| `agent-93e17a3a2800-r01-rv64-ktest` | RV64 SMP, `KREPEAT=1` | 30/30 PASS | 136.346 s |
| `agent-93e17a3a2800-r02-la64-ktest` | LA64 SMP, `KREPEAT=1` | 30/30 PASS | 133.942 s |
| `agent-93e17a3a2800-r03-rv64-preliminary` | RV64 `mask=0x003` | 312/314 | 349.772 s |
| `agent-93e17a3a2800-r04-la64-preliminary` | LA64 `mask=0x003` | 308/314 | 355.234 s |

RV64 仅两套 busybox `kill 10`；LA64 仅两套 `test_brk` 各 1/3 与两套 busybox
`kill 10`，失败身份与 B44 基线一致。四个 QEMU child 均 exit 0、无 panic/timeout/
forbidden marker，且 HEAD、tracked diff 和工作树状态 before/after 一致，
`mutation_detected=false`。

## 未覆盖项

现有 focused 与 basic/busybox 不保证触发 LA64 `AddressNotAligned`，所以只能证明主
trap/signal/clone/exec 路径无回归，不能证明 2/4/8 字节整数和浮点未对齐模拟均已运行。
未来修改该模拟语义时应补专门用户探针；本次不为纯借用重构加入临时生产测试状态。
