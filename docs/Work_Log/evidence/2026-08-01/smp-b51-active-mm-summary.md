# SMP B51 精确 active MM 驻留与安全切离证据

## 1. 结论

状态：`pass`

B51 将用户 MM 的目标 CPU 集合从“曾经使用过该 MM”的单调历史集合，收紧为协议意义上的
精确 active 集合。CPU 只有在完成 enter 后、尚未完成 leave 前才属于该集合；PTE writer、
enter 和 leave 均以同一个 `AddressSpace` 锁为线性化点。已经切离的 CPU 不再接收无意义的
shootdown，但旧 ASID 翻译仍由 generation catch-up 约束，不能未经本地失效重新使用。

本节点没有新增任务状态、TLB pending 层或第二套提交对象。用户 PTE 修改仍沿用
`record_change -> seal -> execute`，普通用户任务也仍保持 CPU0-only，未越过共享
FS/net/driver 的 Phase 5 门禁。

## 2. 设计依据

- [Linux cache/TLB flushing 文档](https://cdn.kernel.org/doc/html/latest/core-api/cachetlb.html)
  将地址空间在哪些 CPU 上执行过/正在执行作为 TLB 失效目标优化的基础。
- [Linux membarrier 文档](https://docs.kernel.org/scheduler/membarrier.html) 要求调度切入、
  切出与 expedited barrier 之间存在完整内存屏障。
- [Linux x86 TLB 实现](https://github.com/torvalds/linux/blob/master/arch/x86/mm/tlb.c)
  以 `switch_mm_irqs_off()`/`leave_mm()` 管理 CPU 当前加载的 MM。

MangoCore 没有 CPU hotplug、lazy-TLB 或 Linux runqueue 上的完整 MM 状态机，因此没有照搬
具体数据结构，而是用已有 VM 锁排序 active mask、generation 和页表修改。

## 3. 状态与调用链

```text
trap_return
  -> ProcessControlBlock::activate_user_vm()       取得共享 VM Arc
  -> processor::switch_user_vm(vm)                 交换 per-CPU 旧/新 MM
  -> old AddressSpace::deactivate_on(cpu)           必要时切离旧 MM
  -> new AddressSpace::activate_on(cpu)             锁内取得 token/ASID
  -> TlbContext::activate_cpu(cpu)                  登记 bit、追赶 generation

task schedule/block/exit
  -> __switch 返回 idle 栈
  -> leave_user_vm(cpu)
  -> AddressSpace::deactivate_on(cpu)
  -> full fence + clear active bit
  -> clear current / finish_switch_out
```

`CpuTaskState.active_user_vm` 保存 `Arc<AddressSpace>`，而不是 MM ID 或重新读取
`process.vm()`。这是因为 exec 可以在当前 syscall 中先替换 PCB 的 VM；per-CPU Arc 必须继续
指向旧 MM，才能在 trap-return 切换时清除正确的 active bit。

槽锁只负责 `take()/store Arc`。它在取得 VM 锁、触发 ASID rollover 或等待 IPI 前已经释放，
因此没有引入 `active_user_vm -> VM -> IPI ack` 的嵌套锁链。

## 4. writer / enter / leave 竞态证明

三条路径均持有同一个 `AddressSpace.inner`，因此只需讨论线性化后的先后次序：

| 先后顺序 | writer 目标 | 必须发生的失效 | frame 何时可释放 |
|----------|-------------|----------------|------------------|
| enter → writer | mask 包含该 CPU | 本地或远端 shootdown，等待 ack | ack 后 |
| writer → enter | writer 可不包含该 CPU | enter 看到新 generation，使用页表根前本地补刷 | writer 锁外提交后；enter 在使用前补刷 |
| writer → leave | writer 快照仍包含该 CPU | 即使 CPU 随后切离，本轮仍等其 ack/STOP | ack 后 |
| leave → writer | mask 不含该 CPU | 不发 IPI；writer 仍推进 generation | 提交后；CPU 下次 enter 前补刷 |

active bit 的清除发生在完整屏障之后、current owner 改变之前。bit 尚未清除的短窗口仍按
“逻辑 active”处理，只可能多发一次 IPI，不会漏失效。bit 清除后，旧任务若再次运行必须
重新经过 `switch_user_vm()`，不能直接使用旧页表根。

## 5. 零目标与 frame 生命周期

`targets == 0` 只证明没有 CPU 当前有资格直接返回该 MM，不证明硬件 TLB 没有旧 ASID 项。
因此 `MmuGather::seal()` 仍执行：

```text
PTE change -> generation++ -> unlock VM -> no IPI -> release retired frame
```

若另一个 CPU 已经开始 enter，它必须等 writer 释放同一 VM 锁；获得锁后会看到新 generation，
先做本地全用户失效，再取得页表根。OOM 应急退休同样保留 `FlushRange` 到 `seal()`：即使 frame
在 VM 锁内提前释放，任何 inactive CPU 也不能在 generation 发布前越过该锁重新进入。

`cpu_tlb_is_current()` 只用于 focused 诊断 observed/generation；生产目标选择、frame 释放和
ack 决策都不依赖这个布尔查询。

## 6. 测试发现与修正

### 6.1 membarrier 假红

首轮 RV64 `membarrier_reaches_mm_cpus` 为 32/33，LA64 为 33/33。helper 在等待窗口调用
`run_task_safe_point()`，可能被 timer 合法切离 MM；CPU0 随后按精确 active mask 不再向它
发 IPI，但旧测试仍要求 request 增长。LA64 通过只是时序差异。

修正后 helper 保持本地 IRQ 开启以响应 IPI，但不在目标观察窗口主动调度。该用例专门覆盖
“目标保持 active 时必须收到 IPI”的分支；新的 `inactive_mm_catches_up_on_wake` 则独立覆盖
“leave 后无 IPI、wake 后 generation 补刷”的分支。

### 6.2 group-exit 假红

首次 `KREPEAT=2` 时 RV64/LA64 都为 64/65，唯一失败均是第二轮
`group_exit_stops_remote_sibling`。最后线程先发布 `live_threads=0` 与 TCB `Zombie`，再调用
PCB `finish_exit()`；测试只等待前两层就立刻断言进程终态。

修正仅把 `process.is_zombie()` 纳入既有完成循环，没有改变生产退出顺序、timeout 或 repeat。
最终双架构两轮均通过。

## 7. 冻结源码与环境

- 分支：`smp`
- 被测 HEAD：`c87d6b5467b1aea4322589707884305f7175f0fd`
- 最终生产/测试 tracked diff SHA-256：
  `c0e54db406bce69031947d152b5502b615f26f747cb647a690256e9ffd5be1e8`
- Docker container：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- Image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- QEMU：RV64/LA64 均为 10.0.2。
- 验证后只新增文档、调试参考和一行源码注释，可执行逻辑未改变。

## 8. 双架构构建与 focused

生产 diff 的独立 normal kernel build：

| 架构 | child job | 结果 |
|------|-----------|------|
| RV64 | `agent-2acbc713d409-r01-rv64-kernel-build` | PASS，exit 0 |
| LA64 | `agent-2acbc713d409-r02-la64-kernel-build` | PASS，exit 0 |

最终 current diff 的 focused：

| 架构 | child job | 配置 | 结果 | 用时 |
|------|-----------|------|------|------|
| RV64 | `agent-8e2b8d48989e-r01-rv64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=2` | 65/65 PASS | 137.878 s |
| LA64 | `agent-8e2b8d48989e-r02-la64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=2` | 65/65 PASS | 139.547 s |

每架构的 TAP 计划本身已经包含两轮，共 65 个检查点；双架构合计 130 个，不再额外乘一次
repeat。两项均 `online_mask=0xff`、exit 0、无 panic/timeout/forbidden marker，且
`mutation_detected=false`。

## 9. 初赛非回归

| 架构 | child job | CORE_NUM | 得分 | 精确失败集合 | 用时 |
|------|-----------|----------|------|--------------|------|
| RV64 | `agent-d3cd9924dd16-r01-rv64-preliminary` | 8 | 312/314 | musl/glibc `busybox kill 10` 各 0/1 | 343.974 s |
| LA64 | `agent-d3cd9924dd16-r02-la64-preliminary` | 8 | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 | 357.132 s |

两项均 `mask=0x003`、`online_mask=0xff`、exit 0、无 panic/timeout/forbidden marker，
`mutation_detected=false`；失败身份与 B50 基线完全一致。

## 10. DeepSeek 结论的人工裁决

- **接受：** 槽锁不跨 VM/rollover/IPI、writer/enter/leave 共同锁序、exec 旧 MM Arc、
  focused 与初赛总体无回归等结论，与源码和原始日志一致。
- **纠正：** active=0 并不代表硬件中没有旧 TLB；安全性来自“此刻不可使用 + 下次 enter
  必须追代”。DeepSeek 还把每架构已含 `KREPEAT=2` 的 65 个 TAP 点再次乘以 2，错误写成
  双架构 260；原始 TAP 是双架构合计 130。
- **不接受：** 没有采用延长 group-exit timeout 或机械重试建议。真实缺口是测试没有等待
  PCB 最终发布点，修正谓词后保持原 timeout/repeat 即通过。

## 11. 已知边界

- 多页连续 range 仍升级为整个用户 MM 的全量失效；B51 只消除已经切离 CPU 的无意义目标。
- 普通用户任务默认仍是 CPU0-only；FS/net/driver、uaccess 与共享子系统审计完成前不开放
  默认全核 affinity。
- STOP/panic 后 active bit 可能保留到不可返回的停机状态；当前不支持 CPU hotplug，STOP
  ack 可以等价证明该 CPU 不再使用旧翻译。
- 未新增专门的退休 Vec OOM 注入测试；现有代码以 fail-stop/泄漏优先保证不提前复用 frame。
