# B85 真实并发 PTE writer 证据

- 状态：`pass`
- 基线 HEAD：`f83d6c349198742d7748dba7b14a911a6a1f0f28`
- 冻结 tracked diff SHA-256：`b90377b86a59af7564827170d528d771938f0cf3380a88b80f6903e9cca3af7b`
- Docker container：`mangocore-smp-integration-20260725-os-dev-1`
- QEMU：RV64/LA64 均为 `10.0.2`

## 验收对象

旧用例只直接提交多个 `UserTlbCommit`，可以证明固定 shootdown slot 不串 payload，却绕过
真实 VM 锁、PTE 修改和 `MmuGather`。B85 将其替换为完整生产路径：

1. 建立一个裸 `AddressSpace`，按 CPU 分配互不重叠的常驻 `MAP_SHARED` 页；
2. CPU0 和所有 AP 先分别 `activate_on(cpu)`，共同 active mask 在整个写入期保持不变；
3. 每个 CPU 在自己的页上经 `AddressSpace::write()` 交替执行 8 轮 `mprotect(R -> RW)`；
4. PTE 修改由 VM 锁串行化，`TlbFlush::execute()` 在锁外运行，因而允许多个 generation、
   range payload 和 ack 等待真实交错；
5. 全部 writer 完成后才撤销 active bit，并检查每核 observed generation 已追上、active mask
   已清零、精准 range 请求没有退化为 full-user flush。

完成屏障临时开放本地中断，使等待中的 CPU 仍能处理其他 writer 发来的 TLB IPI，避免形成
“等待 writer 完成—writer 等待本核 ack”的环形依赖。用例结束后的全部 SMP 测试继续通过，
也证明没有遗留 slot、pending IPI 或 MM residency 状态。

## DeepSeek 冻结验证

任务：`smp-b85-concurrent-pte-r1`

| 顺序 | child job | 配方 | 结果 |
|---:|---|---|---|
| 1 | `agent-c50d911b050d-r01-rv64-kernel-build` | RV64 normal build | PASS，135.9s |
| 2 | `agent-c50d911b050d-r02-la64-kernel-build` | LA64 normal build | PASS，141.8s |
| 3 | `agent-c50d911b050d-r03-rv64-ktest` | RV64，`CORE_NUM=8 KTEST=smp` | PASS，34/34，141.2s |
| 4 | `agent-c50d911b050d-r04-la64-ktest` | LA64，`CORE_NUM=8 KTEST=smp` | PASS，34/34，143.7s |

目标 `smp::concurrent_pte_updates_keep_shootdowns_separate` 在两架构均为 `ok 25`；后续
#26—#34 也全部通过。日志中没有 panic、timeout、fatal trap、active-MM 遗留、generation
落后或 range-to-full fallback。B84 用例中出现的 StorePageFault/PageModifyFault 是预期的
mprotect 降权结果，不是 B85 异常。

四个 child 均 `mutation_detected=false`。一次额外的自然语言只读审查调用在六分钟内没有
产生输出，已终止且不作为证据。DeepSeek prompt、manifest 与完整 stdout/stderr 只保存在
本地忽略的 `cc-codex/`，不纳入 Git 或上传。
