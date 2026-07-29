# SMP B25 LoongArch MM-owned ASID/epoch 证据摘要

## 1. 范围与源码指纹

- 分支：`smp`
- 基线 HEAD：`c19cf61fd0989ede4519aa22af865ba06e169b89`
- 状态：`pass` — 双架构 8 核 focused 通过，双架构 8 核初赛失败集合未扩大
- 测试冻结时 tracked diff SHA-256：
  `7198a1e63b4a147d1dfe2dd622afc9b021993696c8558ef10318107e2b1ad943`
- 四个子任务的 HEAD、status、tracked diff、untracked content 指纹前后一致，
  `mutation_detected=false`。
- 首轮普通用户回归的 tracked diff SHA-256：
  `c1298dc3e8ffd1f93e8aa3fdb7184386f66a25e2af31e77f571e3700d797b9b7`。
  它在 LA64 上稳定暴露 trap-return ABI 故障。
- 修复后 LA64 初赛冻结时 tracked diff SHA-256：
  `ee27d6c0362d5dd8c222030913cf43af98064a98a867300addf1d21433c0eaed`，
  前后完全一致，`mutation_detected=false`。
- 文档冻结后的最终 LA64 focused tracked diff SHA-256：
  `48372c77fd3daf5d456a6e7f9fd140990de5e7a4b0aff1f078340832e71f6beb`；
  最终生产源码 `git diff -- os/src` SHA-256：
  `7867728b9d18759809f18b11ed17ee2457404db216bef60629a06ec390371164`。

## 2. Docker 环境

- 容器：`mangocore-smp-integration-20260725-os-dev-1`
- container ID：
  `a99062375fdbde7b8989f6b9622438229a8609991a3aad86443a5eafcc4acfca`
- image：`zhouzhouyi/os-contest:20260510`
- mount：`/home/lzm/projects/MangoCore-smp-integration-20260725 -> /app`
- toolchain：`nightly-2026-05-10`
- RV64/LA64 QEMU：`10.0.2`

编译严格串行。Worker 通过受限 Docker recipe 执行的实际 facade 为：

```text
make -C os ARCH=rv64 MODE=release PROFILE=normal BUILD_ROOT=/app/build kernel
make -C os ARCH=la64 MODE=release PROFILE=normal BUILD_ROOT=/app/build kernel
make -C os ARCH=rv64 MODE=release PROFILE=normal BUILD_ROOT=/app/build ktest-run
make -C os ARCH=la64 MODE=release PROFILE=normal BUILD_ROOT=/app/build ktest-run
```

focused 两项均注入 `CORE_NUM=8 KTEST=smp KREPEAT=1`，单用例 timeout 为 5000 ms，
QEMU 外层 timeout 为 30 s。

初赛 recipe 注入 `mode=run, mask=0x003, skip_apk=true`，使用 `CORE_NUM=8`，依次构建
normal kernel、生成派生测试盘并运行比赛式 QEMU；外层 timeout 为 900 s。

## 3. 实现不变量

```text
trap_return
  -> ProcessControlBlock::activate_user_vm
  -> AddressSpace::activate_on
       -> TlbContext::assign_asid
       -> TlbContext::activate_cpu
  -> UserVmContext { token, asid }
  -> LA64 __restore 成对安装 PGDL/ASID
```

- ASID 由共享 MM 的 `TlbContext` 持有，不再由 TCB 分配/释放；同一 MM 的线程和 CPU
  必须得到同一个非零硬件 ASID。
- 低 10 位是硬件 ASID，高位是软件 epoch；软件 epoch 不写入 CSR。
- 同一 epoch 内编号只增不减，MM 析构不立即复用。
- 耗尽时 leader 先对全部 online CPU 执行 user-TLB flush/ack，再推进 epoch；等待者
  不持 VM 锁并能开放本地中断响应 shootdown。
- 普通 LA64 context switch 只更新 PGDL/ASID，不再固定执行 `invtlb 0x3`。
- `trap_return()` 把 `__restore` 的固定 ABI 参数直接约束为
  `$a0=trap context, $a1=token, $a2=ASID`；跳转目标使用独立寄存器。不能用多个
  泛型 `in(reg)` 再在 asm 模板内逐个 move，因为编译器不会分析模板内部的覆盖关系。
- trap-return 在最终 `ertn` 前保持本地 IRQ 关闭。因此远端 CPU 若已经取得旧 epoch
  快照，就不能先处理 rollover IPI/ack、再使用该快照返回用户态；这闭合了 flush-before-
  reuse 与返回窗口之间的竞态。

## 4. 初始 focused 验证

DeepSeek job `smp-b25-asid-validation-20260729` 自主选择并串行完成四项验证；GPT/Codex
独立核对每个 child result、QEMU 完成标记与源码指纹：

| 子任务 | 结果 | 用时 | 关键事实 |
|---|---|---:|---|
| `agent-4f5e3e87b73e-r01-rv64-kernel-build` | PASS | 129.791 s | exit 0；RV64 ASID=0 统一接口编译通过 |
| `agent-4f5e3e87b73e-r02-la64-kernel-build` | PASS | 142.655 s | exit 0；epoch allocator、trap/汇编接口编译通过 |
| `agent-4f5e3e87b73e-r03-rv64-ktest` | PASS 19/19 | 133.866 s | configured=8、online=0xff、`KTEST RESULT: PASS` |
| `agent-4f5e3e87b73e-r04-la64-ktest` | PASS 19/19 | 141.506 s | `user ASIDs: 1023`、configured=8、online=0xff、`KTEST RESULT: PASS` |

新增用例的动态证据：

- `address_space_owns_asid`：LA64 同一 `AddressSpace` 在 CPU0/CPU1 获得同一个非零
  ASID；RV64 统一接口稳定返回 0。
- `loongarch_asid_rollover_flushes_before_reuse`：自然耗尽硬件编号后 rollover count
  恰好增加 1；CPU1 的 user-TLB request 增加；旧 MM 在新 epoch 能重新取得非零 ASID。
- 既有 full/page shootdown、ack 前 frame 不释放、kernel-stack reclaim 和 STOP 用例
  在两架构上继续通过。

两份完整 QEMU stdout 保留在本地忽略的 `cc-codex/`，不进入 GitHub；其 SHA-256 为：

- RV64：`2a71e85f24e6b88a2c5b39fe909f1e356707cadb64420f1acba58f270fe66b0b`
- LA64：`b3667f09656e15baca41be029d40c27811d3f9f2acdfb27691f5ddc7289dd0bf`

## 5. 普通用户路径 RED、定因与 GREEN

首轮 DeepSeek job `smp-b25-preliminary-final-20260729` 在同一冻结源码上串行执行双架构
`CORE_NUM=8 mask=0x003`：

| 架构 | 结果 | 动态事实 |
|---|---|---|
| RV64 | PASS，312/314 | 四组完整结束；只有 musl/glibc `busybox kill 10` 两项既有差异 |
| LA64 | RED | 启动正常、online=0xff；同步时钟后出现 `PageInvalidStore bad addr=0x13b`、`PageInvalidFetch bad addr=0`，四组均未开始 |

Codex 没有根据 fault 表象修改页表或恢复 context-switch 全刷，而是检查 B25 前后最终
release ELF。故障二进制在 `trap_return()` 尾部生成：

```text
bstrpick.d a0,s1,15,0   # ASID 暂存在 a0
move       a0,s2        # a0 被 trap context 覆盖
move       a1,s0        # token
move       a2,a0        # 错把 trap context 当 ASID
```

根因是原 Rust `asm!` 把四个值都声明为泛型 `in(reg)`，然后在模板内顺序搬到
`$a0/$a1/$a2`。LLVM 只保证输入在 asm 开始时有效，不知道前一条 `move` 会覆盖后续输入，
因此允许 ASID 与 `$a0` 复用。

修复后显式绑定三个 ABI 参数。Docker normal build 生成的最终 ELF 为：

```text
bstrpick.d a2,s2,15,0   # ASID 直接进入 a2
move       a0,s1        # trap context
move       a1,s0        # token
jr         a3           # restore 地址与参数寄存器分离
```

DeepSeek job `smp-b25-la64-asm-fix-validation-20260729` 随后得到：

| 子任务 | wrapper 结果 | 人工裁决 |
|---|---|---|
| LA64 normal build | PASS，exit 0 | ABI 约束可由 pinned toolchain 正常编译 |
| LA64 8 核 focused | wrapper FAIL | QEMU 实际 19/19；Codex 在运行中编辑 tracked workflow 文档触发 `mutation_detected=true`，不作为最终验收证据 |
| LA64 8 核初赛 | PASS，exit 0 | 四组完整结束，308/314；失败集合与既有 LA64 基线一致；无 page-invalid、panic、timeout |

这里明确纠正 DeepSeek 报告中的归因：focused 的 mutation 不是“构建产物改变 Git 状态”，
而是 Codex 并行更新了 `.agents/skills/mango-workflow/references/debugging-patterns.md`。
wrapper 拒绝该轮是正确行为，因此在文档冻结后另行重跑 focused。

最终 DeepSeek job `smp-b25-la64-focused-freeze-20260729` 只允许一次 `la64-ktest`：
exit 0、139.396 s、configured=8、online=0xff、19/19、`KTEST RESULT: PASS`；
required marker 无缺失、forbidden marker 为空，且 HEAD/status/tracked/untracked 三组指纹
前后一致，`mutation_detected=false`。这才是修复后 focused 的最终验收证据。

本地完整 stdout（按协作边界不提交 GitHub）的 SHA-256：

- 首轮 RV64 初赛 PASS：`68913c05021b8f059025bcb762fc0aca4ea2c08be9109527569745353932817d`
- 首轮 LA64 初赛 RED：`ae6abbf28797f47a4503647459a1a683d57cdbf1b88b13c043b39557a5782316`
- 修复后 LA64 初赛 GREEN：`5f7f5a2fe5c8a2eff4aa13af9c9b0694cc6a51c01ca804c4be6e2b22abc069bb`
- 最终 LA64 focused：`b3667f09656e15baca41be029d40c27811d3f9f2acdfb27691f5ddc7289dd0bf`

## 6. 人工裁决与未覆盖范围

- `rollover_asids()` 的 VM 锁顺序已人工复核：`activate_on()` 在进入 flush/ack 前离开
  `inner.lock()`；leader/waiter 均不把普通锁带过等待点。
- rollover flag 在全局 flush 前发布。已在用户态的 CPU 必须先 flush 再 ack；处于
  trap-return IRQ-off 窗口的 CPU 必须先完成返回才能处理 IPI，因此不会在 ack 后用旧
  snapshot refill；尚未取得 snapshot 的 CPU 会看到换代或 epoch 变化后重试。
- `__restore` 删除每次 context switch 的全刷，依据是编号在一个 epoch 内唯一且编号
  复用前已经完成全 CPU flush，而不是仅依据“测试通过”。
- 本批已经运行双架构 `CORE_NUM=8 mask=0x003`：RV64 312/314、LA64 308/314，四组完整且
  失败集合没有扩大。它验证真实 trap-return/exec/syscall 路径，但不等价于用户任务跨核；
  普通用户 affinity 解除后仍须重新执行该门禁。
- 尚未覆盖 LA64 ASID+VPN 精确 shootdown、连续 range、CPU detach、用户任务跨核迁移
  和 ASID epoch 的理论 `u64` 溢出边界。
