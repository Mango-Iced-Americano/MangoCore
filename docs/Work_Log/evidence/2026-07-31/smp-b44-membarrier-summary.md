# SMP B44 membarrier 实施证据

状态：`pass`

## 变更边界

- PRIVATE_EXPEDITED 注册状态从 TCB 下沉到共享 `AddressSpace`，使 `CLONE_VM`
  线程共享注册；fork/exec 的新 MM 自然回到未注册状态。
- `smp` 增加独立的 memory-barrier request/ack 与 IPI reason。它只承载 full
  memory fence，不复用 TLB generation、shootdown payload 或 `MmuGather`。
- GLOBAL 面向全部 online CPU；PRIVATE 在 VM 锁内冻结历史 cached CPU mask，
  解锁后才等待远端 ack。
- IPI handler 只执行原子 load、full fence 和原子 ack，不分配、不取普通锁。

## 竞态与锁序证明

PRIVATE 目标快照与 `AddressSpace::activate_on()` 共用 VM 锁：

1. 新 CPU 先激活：它先登记到单调 mask，随后被快照捕获并接收 IPI；
2. PRIVATE 先快照：新 CPU 随后首次登记，并在使用该 MM 前执行本地 full fence。

发送侧固定执行 `pre fence -> Release 发布 request/reason -> doorbell -> Acquire
读取 ack -> post fence`。多个发送者对同一 CPU 使用单调 request 序号；reason bit
即使合并，handler 读取最新 request 并一次 ack 到该序号，较早等待者也可完成。
等待使用 `IpiWaitIrqGuard` 临时开放本地中断，不持 VM、task.inner、runqueue 或
其它普通锁。当前无 CPU hotplug；目标进入不可返回 STOP 后，其终态 ack 可替代本轮
barrier ack。

## DeepSeek 只读审查与裁决

冻结审查 `smp-b44-memory-order-review-r2` 确认 MM-owned 生命周期、VM 锁两种竞态
次序、request/ack 合并、历史 mask 超集和 STOP/锁序均成立。采纳了两项可维护性建议：

- 在 `activate_cpu()` 注释中同时写明首次 membarrier fence 与 TLB generation
  追赶为何必须共用 VM 锁；
- focused test 精确核对每个目标的 request 只增加一次，且 ack 不落后。

审查报告另有一项把 `MEMBARRIER_CMD_GLOBAL = 1` 误认为 QUERY 的意见，未采纳。
Linux UAPI 中 QUERY 是 `0`，GLOBAL 是 `1 << 0`；实现和测试均按该定义。
并发两个 syscall 发送者的确定性压力注入没有加入本批，避免为测试再增加生产状态；
序号合并由源码不变量和现有并发 IPI reason 用例覆盖，后续长测仍应观察超时计数。

## 环境与源码指纹

- 被测 HEAD：`9cc10dc7005390aa8b24c8d912f8cacc3b66d9ec`
- 最终可执行源码 diff SHA-256：
  `a1af1e31042d8055a8c040c3fb1ef835351334f2b4211237eaac46f45335b051`
- Docker image：`zhouzhouyi/os-contest:20260510`
- image ID：
  `sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- repo digest：
  `sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2

## 验证结果

| Job | 配置 | 结果 | 用时 |
|-----|------|------|------|
| `smp-b44-rv64-build-r2` | RV64 normal, 8 核 | PASS | 130.326 s |
| `smp-b44-la64-build-r2` | LA64 normal, 8 核 | PASS | 131.385 s |
| `smp-b44-rv64-focused` | RV64 SMP, `KREPEAT=2` | 59/59 PASS | 137.444 s |
| `smp-b44-rv64-final` | RV64 SMP, final diff | 30/30 PASS | 137.168 s |
| `smp-b44-la64-final` | LA64 SMP, final diff | 30/30 PASS | 136.312 s |
| `smp-b44-rv64-preliminary` | RV64 `mask=0x003` | 312/314 | 348.201 s |
| `smp-b44-la64-preliminary` | LA64 `mask=0x003` | 308/314 | 357.023 s |

最终 focused 与两项初赛任务的 HEAD、status 和 tracked diff before/after 完全一致，
`mutation_detected=false`。新增 `smp::membarrier_reaches_mm_cpus` 在双架构均直接
PASS；它经 syscall 分发验证 QUERY、注册、未注册 EPERM、PRIVATE 的 MM CPU 目标以及
GLOBAL 的全部 AP ack。

初赛失败集合未扩大：RV64 仍仅两套 busybox `kill 10`；LA64 仍为两套
`test_brk` 各 1/3 与两套 busybox `kill 10`。无 panic、timeout 或 forbidden marker。
独立 build 使用的早期 diff 只比最终版本少测试诊断 accessor 和注释；最终 focused
任务已在精确最终 diff 上重新编译双架构内核。

## 已知边界

- cached CPU mask 仍单调增长，PRIVATE 可能向已经离开 MM 的 CPU 发送多余 IPI；
  这是性能成本，不是遗漏目标的正确性问题。
- MangoCore 当前不支持 CPU hotplug；实现不能直接外推到可重新上线的 stopped CPU。
- advertised membarrier 若无法完成 IPI/ack 会 fail-stop，不会向用户空间伪报成功。
