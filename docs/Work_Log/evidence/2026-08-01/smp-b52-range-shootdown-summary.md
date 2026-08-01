# SMP B52 有界连续用户 TLB shootdown 证据

## 1. 结论

状态：`pass`

B52 在不增加第二条 MM 提交链的前提下，将用户页表失效从“单页精准、第二页即全刷”扩展为
“最多 64 页的连续半开 VPN 区间精准失效”。`MmuGather` 仍在 VM 锁内收集变化，
`TlbFlush` 仍在解锁后执行本地/远端失效并等待 ack，退休 frame 仍只在同步完成后释放。

本节点完成的是区间后端、并发 payload 隔离和 frame 生命周期协议。它尚未用持续运行的
用户 load/store victim 动态证明 stale translation 已不可见，因此 Phase 4 仍不宣称全部完成。

## 2. 设计依据

- [RISC-V SBI RFENCE 扩展](https://docs.riscv.org/reference/sbi/ext-rfence.html)规定
  `remote_sfence_vma_asid` 接收 hart mask、起始地址、字节大小和 ASID，并在调用返回前完成
  目标 hart 的区间失效。
- [Linux RISC-V `tlbflush.c`](https://github.com/torvalds/linux/blob/master/arch/riscv/mm/tlbflush.c)
  同样按 start/size/stride 表达范围，并以阈值决定逐页还是全刷。MangoCore v1 取 64 页作为
  hard-IRQ 工作量上界，而不是照搬 Linux 的完整 mm_cpumask、broadcast TLB 或 CPU hotplug。
- [LoongArch 架构手册](https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html)
  规定 `INVTLB op=0x5` 按 `G=0 + ASID + VA` 过滤；普通 TLB entry 覆盖相邻偶/奇页，所以
  MangoCore 从偶数 VPN 边界开始每两页执行一次。

## 3. 保持不变的主调用链

```text
AddressSpace::write
  -> lock VM
  -> UserMapper 修改 PTE
  -> MmuGather::record_change / retire_frame
  -> MmuGather::seal
  -> unlock VM
  -> TlbFlush::execute
       -> local range/full flush
       -> remote RFENCE or fixed slot/full IPI
       -> wait for completion
       -> acknowledge generation
       -> release retired frames
```

没有新增 `commit/pending/batch` 类型。`FlushRange` 只有 `None / Range / Full`：

- 首个 VPN 建立 `[vpn, vpn + 1)`；
- 后续 VPN 与已有范围取最小包围区间，重复页不改变范围；
- `vpn + 1` 溢出、跨度超过 64 页或页表层级变化时进入吸收态 `Full`；
- 区间内未修改的空洞允许被额外失效，避免维护动态离散列表。

## 4. 双架构执行

### RV64

- 本地范围逐页执行 `sfence.vma va, asid`。
- 远端范围把逻辑 CPU mask 转为物理 hart mask，并把 VPN 半开区间转换为字节
  `start/size` 后调用 SBI RFENCE FID 2。
- 固件没有 RFENCE 时使用与 LA64 相同的固定区间 slot；不静默改成无同步本地刷新。

### LA64

- 固件层返回“不支持远端 RFENCE”，由软件 IPI slot 承载 payload。
- 起始 VPN 向下对齐到偶数，之后每次增加 2，覆盖半开区间相交的全部硬件页对。
- 奇数起点/终点只会额外失效相邻一页，不会遗漏目标页。

## 5. 固定 slot 与 IRQ 上界

每个发起 CPU 独占一个 `UserTlbRangeSlot`，字段为 claimed、targets、acknowledged、ASID、
start VPN 和 page count。发起者先写 payload，最后以 Release 发布 targets；handler 以
Acquire 观察 targets 后读取 payload，完成硬件失效，再以 Release 写 ack。

一个合并的 IPI reason 可以服务全部固定槽，但不同发起者不会覆盖彼此 payload。handler
最多扫描 `MAX_CPUS=8` 个槽，每槽最多处理 64 个逻辑页；不分配、不获取普通锁、不等待。
槽超时后不复用，避免迟到 doorbell 与下一轮 payload 发生 ABA 错配。

## 6. 冻结源码与环境

- 分支：`smp`
- 被测 HEAD：`3a345cf1843657ce572e638c512facd84edaa0f0`
- 生产/测试 tracked diff SHA-256：
  `1d8909e1a37843be5673affe6fe6b0952076a54f0acb9ba9f7a2a687047a2c43`
- Docker container：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- Image ID：`sha256:60e9bfa0ecdc6be93d9beb6b1d249f34163b08e32e97f090590a93a92e9357ac`
- Repo digest：
  `zhouzhouyi/os-contest@sha256:85dec949df7cef41fd03d30c6ad69f952204540e18d2c62bced9d2e262fef12d`
- RV64/LA64 QEMU：10.0.2。

所有 child 的 source-before/source-after 指纹一致，`mutation_detected=false`。

## 7. 只读并发审查

本地只读 worker 任务 `smp-b52-range-design-review` 返回 PASS。重点核对：

- `record_change()` 的重复 VPN、稀疏合并、溢出和 `Range -> Full` 吸收态；
- slot 的 Release/Acquire 发布、ack 和复用时序；
- hard-IRQ 固定 8 槽、最多 64 页的工作量上界；
- RV64 的 hart mask/start/size/ASID 与 LA64 的奇偶页覆盖；
- RFENCE、slot、全刷 fallback 和 frame 退休的先后顺序；
- 命名、中文注释和分层未重新引入 B23 清理过的重复提交概念。

GPT/Codex 逐项对照源码和原始日志后接受审查结论；模型自报版本不作为工具身份或正确性证据。

## 8. 双架构构建与 focused

| 架构/任务 | child job | 配置 | 结果 | 用时 |
|-----------|-----------|------|------|------|
| RV64 build | `agent-e4697cd353f9-r01-rv64-kernel-build` | `CORE_NUM=8` normal kernel | PASS，exit 0 | 135.304 s |
| LA64 build | `agent-e4697cd353f9-r02-la64-kernel-build` | `CORE_NUM=8` normal kernel | PASS，exit 0 | 135.150 s |
| RV64 focused | `agent-e4697cd353f9-r03-rv64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=1` | 33/33 PASS | 137.695 s |
| LA64 focused | `agent-e4697cd353f9-r04-la64-ktest` | `CORE_NUM=8 KTEST=smp KREPEAT=1` | 33/33 PASS | 132.947 s |

两项 focused 均打印 `online_mask=0xff`，且以下生产路径用例逐项 PASS：

- 三页 `user_tlb_range_sync_uses_arch_backend`，并断言没有退化为 full request；
- 所有 CPU 同时发布不同三页 payload 的 `concurrent_range_shootdowns_keep_payloads_separate`；
- 65 页主动跨过上限的 `user_tlb_retirement_waits_for_ack`，证明 ack 前不释放 frame。

## 9. 初赛非回归

| 架构 | child job | 拓扑 | 得分 | 精确失败集合 | 用时 |
|------|-----------|------|------|--------------|------|
| RV64 | `agent-e4697cd353f9-r05-rv64-preliminary` | configured=8, online=0xff | 312/314 | musl/glibc `busybox kill 10` 各 0/1 | 358.226 s |
| LA64 | `agent-e4697cd353f9-r06-la64-preliminary` | configured=8, online=0xff | 308/314 | musl/glibc `test_brk` 各 1/3；`busybox kill 10` 各 0/1 | 362.969 s |

两项均使用 `mask=0x003`、exit 0，无 panic、timeout、fatal 或 forbidden marker；失败身份与
B51 基线一致。拓扑由同一 child 的 OpenSBI/QEMU 启动输出和内核 `online_mask` 共同证明，
不以宿主机瞬时进程标题作为验收依据。

## 10. 已知边界

- 64 是当前实现的确定性 IRQ 上限，不是永久 ABI；调整时必须同时验证性能和 handler 延迟。
- 稀疏修改以包围区间多刷少量页；跨度超过 64 页仍全刷。
- focused 测试证明后端选择、并发 payload 和 frame 生命周期，但尚未让持续用户访存直接观察
  PPN/权限改变；后续 stale-PTE 用例必须避免被 timer/context switch 的偶然全刷掩盖。
- 普通用户任务默认仍为 CPU0-only；本节点不扩大 FS/net/driver 的并发可见范围。
