# SMP B53 真实用户访存 stale-TLB 证据

## 1. 结论

状态：`pass`

B53 不再只用 request/ack 计数器间接证明 shootdown。CPU1 先在用户态持续读取旧物理页，
CPU0 再经正式 CoW、PTE、`MmuGather`、`TlbFlush` 主链替换同一 VPN 的 PPN；只有目标 CPU
实际完成 ASID+VPN 区间失效，用户探针才会读到新物理页 canary 并退出。

这次同时修复了一个会掩盖精准后端缺陷的性能/证据竞态：软件 range handler 过去只 ack，
目标从 IPI 返回用户态时可能先看到新 generation，再额外执行一次全用户 TLB 失效。现在
handler 在精准失效之后、ack 之前发布本 CPU observed generation；`activate_cpu()` 不再用
偶然的 full flush 替代本轮精准失效。

## 2. 设计依据

- [RISC-V SBI RFENCE 扩展](https://docs.riscv.org/reference/sbi/ext-rfence.html)规定远端
  SFENCE 调用返回时目标 hart 已完成失效。
- [Linux RISC-V `tlbflush.c`](https://github.com/torvalds/linux/blob/master/arch/riscv/mm/tlbflush.c)
  区分本地、远端、ASID 和范围失效；MangoCore 保留自己的 VM 锁、generation 和固定槽协议。
- [LoongArch 架构手册](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
  规定 `INVTLB op=0x5` 按 non-global、ASID 与 VA 过滤。

## 3. 生产协议变化

`UserTlbRangeSlot` 在既有 `ASID + start_vpn + page_count` 之外保存可空的
`TlbContext` 指针和 generation：

1. 发起者在 VM 锁外持有借用 `TlbContext` 的 `TlbFlush`；
2. payload 字段先写入，最后以 Release store 发布 targets；
3. handler 以 Acquire 取得 targets，执行真实区间失效；
4. handler 单调发布该 CPU 的 observed generation；
5. handler 最后以 Release 发布 ack；
6. 发起者看到全部 ack 后才释放槽和退休 frame。

指针不会越过借用生命周期：正常路径只有 handler 完成访问并 ack 后才返回；timeout 是
fail-stop，槽不再复用，`TlbFlush` 也不会继续正常释放 MM/frame。RV64 SBI RFENCE 没有共享
软件槽，固件同步返回后仍由发送方统一记账。全量失效同样不使用精准槽元数据，完成既有
request/ack 后再由发送方统一推进 observed。

## 4. 不依赖偶然 full flush 的用户证明

用户汇编探针只执行普通 load/store：

```text
CPU1 user: load target == OLD
           fence
           progress = READY
           loop load target while value == OLD

CPU0:      real private COW Store fault
           PTE PPN: old -> new
           range shootdown + remote ack
           write NEW into new frame

CPU1 user: next target load must see NEW
           progress = PASS
           exit(0)
```

测试把 CPU1 本地 timer 静默，只开放 IPI；用户探针之后按 FIFO 放置 timer 恢复 helper。
helper 若在探针给出 PASS/FAIL 前运行，会记录证据污染并使测试失败。测试还要求 full-user
request 计数不变，并要求 handler 已把 CPU1 observed 推进到当前 generation。因此，timer、
context switch 或 trap-return generation catch-up 不能把坏的精准 handler 伪装成 PASS。

旧 frame 由测试额外持有，强制 CoW 进入复制分支；新页内容只在 `AddressSpace::write()`
完成 shootdown 后写入。若旧翻译仍存在，CPU1 会永久读取保留的 OLD，而不会因物理页复用
偶然读到 NEW。内核直映访问只使用瞬时 volatile raw pointer，不与用户硬件访存构造重叠的
Rust `&mut` 引用。

## 5. 冻结源码与环境

- 分支：`smp`
- 被测 HEAD：`16db19a4659c948b290c0ed1e85412ef5ba1961c`
- 最终生产/测试 tracked diff SHA-256：
  `bb213434751e37a470d1dffe70c776c9d66aec5bdec456a237db2e71335aa396`
- Docker container ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- Image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2。

最终四个 child 的 source-before/source-after 指纹一致，`mutation_detected=false`。

## 6. 验证演进与根因闭环

首轮正确容器任务 `smp-b53-validation-r2` 的双架构 build 和初赛均通过；新增用户探针在
RV64/LA64 都先显示 `ok`。随后 65 页全刷 retirement 用例进入
`FlushRange::Full` 时触发新断言：生产 `TlbFlush` 无条件把精准槽 generation 传给了
`range=None`。这不是硬件失效失败，而是接口分支构造错误。

修正后只有 `FlushRange::Range` 携带 `(TlbContext, generation)`，`Full` 传 `None` 并保留
原有全刷记账。保留断言而不是删除它，使未来再次误用时仍能 fail-fast。

| 架构/任务 | child job | 配置 | 结果 | 用时 |
|-----------|-----------|------|------|------|
| RV64 build | `agent-abc54595f65f-r01-rv64-kernel-build` | `CORE_NUM=8` normal kernel | PASS，exit 0 | 131.343 s |
| LA64 build | `agent-abc54595f65f-r02-la64-kernel-build` | `CORE_NUM=8` normal kernel | PASS，exit 0 | 138.888 s |
| RV64 focused | `agent-abc54595f65f-r03-rv64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=2` | 67/67 PASS | 138.136 s |
| LA64 focused | `agent-abc54595f65f-r04-la64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=2` | 67/67 PASS | 132.446 s |

两项 focused 均打印 `online_mask=0xff`；真实 COW 用户探针、并发 range payload 和 65 页
full-flush/frame-retirement 在两轮中全部通过，无 panic、timeout 或 fatal。

## 7. 初赛非回归

首轮冻结源码 `da6a81f1...` 上的双架构 `CORE_NUM=8 mask=0x003` 已完成：

| 架构 | child job | 结果 | 既有失败集合 |
|------|-----------|------|--------------|
| RV64 | `agent-73c54546b2e2-r05-rv64-preliminary` | 312/314，exit 0 | musl/glibc `busybox kill 10` |
| LA64 | `agent-73c54546b2e2-r06-la64-preliminary` | 308/314，exit 0 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` |

最终修正只改变远端 `FlushRange::Full` 是否携带精准槽元数据，普通 CPU0-only 初赛路径不进入
该分支；因此修正轮按风险只重跑双架构 build/focused，没有把首轮初赛冒充为重新执行。

## 8. AI 协作与人工裁决

- `smp-b53-stale-tlb-design` 给出只读设计建议；GPT/Codex 采纳共享进度页和无 syscall
  victim 思路，但拒绝新增 `replace_user_frame_with()` 生产 API，改用现有真实 CoW 主链。
- 设计报告曾把 `cpu_tlb_is_current()==false` 误当成“没有偶然 trap”的依据；实际 writer
  正常完成后本就会推进 observed。最终实现改为 timer 静默、FIFO restore helper、full
  request 不变和 handler 侧 observed 四项联合证据。
- `smp-b53-implementation-review` 超时，明确记为 NOT RUN，不作为通过证据。
- 第一次验证因容器 `/app` 挂载到错误 worktree 被网关 fail-closed，未算源码验证；r2/r3
  改用正确容器。r2 自主定位 full 分支误传参数，GPT/Codex 对照原始 TAP 顺序修正了其
  “#25 失败”的表述：#25 已通过，panic 发生在下一项输出结果之前。

## 9. 已知边界

- 本节点直接证明一页 CoW PPN 替换后的远端用户 load，不等于已经覆盖所有
  `mprotect(PROT_NONE)`、`munmap`、exec 或高并发 fault 模式。
- 普通用户任务默认仍为 CPU0-only；本测试只在受控 CPU1 上运行，不扩大 FS/net/driver
  并发范围。
- 固定槽中的 MM 指针依赖同步等待和 fail-stop timeout。若未来让 timeout 可恢复，必须先
  改成带 sequence/epoch 的可回收所有权协议，不能直接清槽或继续运行。
