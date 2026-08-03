# B95 生产 IPI 协议收口证据摘要

## 范围与结论

本节点删除早期 SMP bring-up 为 ktest 单独保留的 `PING` 和
`ROUND_TRIP_REQUEST/REPLY` 协议。生产 `PerCpu` 不再保存 ping ack、延迟 reply
和 reply ack，AP idle 路径也不再承担测试回包。focused SMP 测试改为直接调用
正式 `synchronize_memory()`，验证 `MEMORY_BARRIER` 的 request/ack。

这不是削弱测试：旧用例只能证明 doorbell 能往返，新用例同时经过生产 mailbox、
完整内存屏障、sequence 发布、远端 ack 和超时处理。

## 生产不变量

- `pending_ipi` 只保存生产 reason；幂等 reason bit 仍不是事件计数器。
- 需要完成语义的 MEMORY_BARRIER、TLB 和 STOP 使用各自的 sequence/ack 或终态 ack。
- BSP→AP 单播和广播直接核对每个远端 CPU 的 request 恰好推进一次，且 ack 已追上。
- AP→BSP helper 在指定 AP 上运行，每个 AP 连续完成 64 轮正式同步；CPU0 只在受控
  IRQ-on 窗口等待，因此能进入 kernel IPI trap 并及时应答。
- helper 发布结果后仍可能使用自己的 kernel stack；测试同时等待 `Zombie` 和目标
  CPU current 槽清空，之后才释放本地 Arc 并从 per-CPU zombie 队列回收。
- reason 数值只存在软件 mailbox 内，没有汇编、硬件 doorbell 或持久 ABI 依赖，删除
  测试 reason 后连续重编号不会改变架构接口。

## DeepSeek 协作与验证

DeepSeek 首先以 max effort 做冻结只读审查，未发现阻塞项，并确认调用 CPU 自动加入
membarrier target 不会掩盖远端方向：BSP→AP 用例仍核对 AP request，AP→BSP 用例仍
要求 CPU0 产生并完成远端 request。随后通过受限 Docker gateway 严格串行执行：

| 架构 | recipe | 结果 | 耗时 | 关键证据 |
|---|---|---:|---:|---|
| RV64 | `rv64-ktest`, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 34/34 PASS | 141.806 s | `online_mask=0xff`，新用例 #5/#6/#9 PASS |
| LA64 | `la64-ktest`, `CORE_NUM=8 KTEST=smp KREPEAT=1` | 34/34 PASS | 136.518 s | `online_mask=0xff`，新用例 #5/#6/#9 PASS |

两项 runner 均 `process_exit_code=0`、`timed_out=false`、
`mutation_detected=false`，无 panic、fatal trap 或 forbidden marker。日志中的 RV64
StorePageFault 与 LA64 PageModifyFault 来自既有远端 mprotect 降权用例，属于预期用户
fault，相关用例最终 PASS。

## 冻结信息与边界

- baseline HEAD: `01ccd4568e36aba8ea5c92eaea93aee187f5e0cf`
- tracked diff SHA-256: `0fdd5afcbfb0546d7cf70953030183237f27a08fbed35f136364f8b70af1fbc2`
- RV64 child: `agent-d9714007633a-r01-rv64-ktest`
- LA64 child: `agent-d9714007633a-r02-la64-ktest`

本证据验证双架构 8 核生产 IPI/membarrier、调度、TLB 和进程停止 focused suite；
不宣称 FS/Net/Driver 已完成 SMP 审计，也不解除普通用户任务的 CPU0 默认 affinity。
