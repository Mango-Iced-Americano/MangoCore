# SMP B22 用户 MM 激活与 user-TLB IPI 基础设施证据摘要

## 1. 证据对象

- 分支：`smp`
- 基线提交：`bac670710e65d5e62f8e27042c7d6111f1cf906e`
- 基线含义：B21 kernel-global 撤映射与内核栈延迟回收
- B22 状态：未提交，等待维护者人工批准
- 生产源码 tracked diff SHA-256：
  `c705122d59ca86c0d838137945e0d793baf13786d76d52f4c4f0c4120a2b37f8`
- 新文件 `os/src/mm/tlb_state.rs` SHA-256：
  `12231db717fbe2d47df9bd94b3037b89a2d6369fb7fff649ba0b1ae2c21de764`
- 本地协作目录：`/home/lzm/projects/MangoCore/cc-codex/`，被 Git 忽略，不进入 GitHub

Docker runner 的 source fingerprint 只包含 HEAD、status 与 tracked diff；因此本摘要另行
记录未跟踪 `tlb_state.rs` 的内容哈希，并在测试结束后再次核对。最终验收不能只依赖
runner 的 `mutation_detected=false`。

## 2. 实现范围

### 2.1 已完成

1. 每个 `AddressSpace` 持有共享 `MmTlbState`：唯一 MM ID、单调 `cached_cpus`、
   generation 和 per-CPU observed。
2. 用户 trap-return 在恢复页表根前，经进程 VM 锁登记当前 CPU。顺序固定为 join mask、
   读取 generation、必要时本地全用户失效、发布 observed、重查 generation。
3. 第二颗 CPU 曾登记后，`TlbPublication` 永久升级为 `Published`；现有 batch 仍在 PTE
   写入前 fail-stop。
4. Per-CPU 增加独立 `user_tlb_request/user_tlb_ack` 与 `USER_TLB_SYNC` reason；不复用
   kernel-global sequence。
5. 提供只允许锁外调用的 `synchronize_user_tlb_mask()`。等待时临时开放本地 IRQ，handler
   只做 request Acquire、本地失效与 ack Release。
6. RV64 采用全量 `sfence.vma`；LA64 采用 `invtlb 0x3` 清除全部 `G=0` 项。
7. focused ktest 调用生产同步原语，核对所有在线 AP 的 user-TLB ack 增长。
8. 人工反馈后将四层同名 `prepare_user_return()` 收敛为 `prepare_return()`、
   `prepare_user_vm()`、`activate_on()` 和 `attach_cpu()`；只改命名与调用点。

### 2.2 明确未完成

- `TlbBatch` 修改侧尚未推进 generation、快照 cached mask 或返回锁外提交对象。
- 未在真实 PTE 撤映射/降权后等待远端 ack；deferred frame 尚未跨 VM 锁保留到远端完成。
- 未开放普通用户任务跨 CPU 执行、迁移或 affinity。
- 未实现 range shootdown、SBI RFENCE 优化、MM-owned LoongArch ASID 或 epoch rollover。
- focused IPI 用例不证明 stale translation、generation race 或 ack 前 frame 不复用。

## 3. 为什么不能直接改现有 `TlbBatch::commit()`

现有 AddressSpace PTE 修改在外层进程 VM `spin::Mutex` 持有期间创建并提交 batch。若
`commit()` 直接等待远端 CPU：

1. 发起 CPU 持 VM 锁等待远端 ack；
2. 目标 CPU 可能已经 IRQ-off 进入 page fault，并等待同一 VM 锁；
3. 目标无法取得锁并离开该窗口，发起者也不释放锁，形成环形等待。

B23 的固定结构应为：VM 锁内修改 PTE、推进 generation、快照目标、转移 deferred frame；
释放 VM 锁；锁外本地/远端失效并等待 ack；最后释放 frame。等待者临时开 IRQ只解决两个
无锁等待者互相成为 IPI 目标，不能修复“持普通锁等待”。

## 4. 内存序与人工裁决

激活侧先 `cached_cpus.fetch_or(AcqRel)`，再 `generation.load(Acquire)`。若反序，修改方可
在两步间推进 generation 并快照不包含新 CPU 的 mask，造成 self-flush 与 IPI 同时漏失。
flush 后必须再读 generation，防止把旧 flush 标记为观察到更新代际。

DeepSeek 最终审查提出：cached mask 与 generation 是不同 Atomic，分别使用
Acquire/Release 不会自动形成跨原子同步；建议 B23 使用 `generation.fetch_add(AcqRel)`。
人工裁决如下：

- 接受“不能假定跨 Atomic 自动传递”的风险提示；
- 不接受“单独改成 AcqRel RMW 就自然闭合全部次序”的结论；
- B23 继续让激活登记与修改方 generation/目标快照共用同一 VM 锁，锁才是真实串行边界；
- 如果未来做 lockless 激活，必须另给两种竞态次序的正式证明与 fence/重试协议。

## 5. 官方架构依据

- RISC-V Supervisor ISA：无地址和 ASID 操作数的 `SFENCE.VMA` 对当前 hart 执行全量地址
  翻译同步；TLB/地址翻译缓存是 hart-local，因此远端 hart 必须通过 RFENCE 或 IPI 各自执行。
  来源：<https://docs.riscv.org/reference/isa/priv/supervisor.html>
- LoongArch Reference Manual Vol. 1：`INVTLB` 操作码 `0x3` 清除全部 `G=0` TLB 项，
  不依赖单个目标 ASID；适合当前 ASID 仍归 TCB 的保守全用户失效。
  来源：<https://loongson.github.io/LoongArch-Documentation/LoongArch-Vol1-EN.html>

## 6. DeepSeek 只读协作

| Job | 结果 | 用时 | 作用 |
|-----|------|------|------|
| `smp-b22-user-mm-design-review-20260728` | `REVIEWED` | 267.691 秒 | 源码调用链、VM 锁死锁、激活顺序、ASID 与最小边界审查 |
| `smp-b22-foundation-final-review-20260728` | `REVIEWED` | 389.085 秒 | 冻结源码最终审查；无 P0，提出 1 个未来 P1，人工按 §4 裁决 |
| `smp-b22-validation-summary-20260728` | `REVIEWED` | 163.506 秒 | 独立读取六个 job，复核 focused、初赛 raw/semantic 与证据外推边界 |

两个任务均为只读，没有修改、commit 或 push 权限。原始 prompt、stdout、analysis 和
result 仅保存在本地忽略的 `cc-codex/runtime/jobs/`。

## 7. Docker/QEMU 结果

所有任务由本地 `cc-test` 后台 runner 在 Docker 中执行，双架构严格串行；GPT/Codex 在
后台运行期间继续做协议审计和文档整理，没有在宿主机重复编译。

| Job | 配置 | 结果 | 用时 | 关键事实 |
|-----|------|------|------|----------|
| `smp-b22-rv64-build-r1-20260728` | RV64 normal build | PASS | 124.734 秒 | exit 0，无源码漂移 |
| `smp-b22-la64-build-r1-20260728` | LA64 normal build | PASS | 130.035 秒 | exit 0，无源码漂移 |
| `smp-b22-naming-rv64-build-20260728` | RV64 normal build，四层方法重命名后 | PASS | 130.577 秒 | exit 0，无源码漂移 |
| `smp-b22-naming-la64-build-20260728` | LA64 normal build，四层方法重命名后 | PASS | 133.933 秒 | exit 0，无源码漂移 |
| `smp-b22-rv64-ktest-r1-20260728` | 8 核，`KTEST=smp KREPEAT=2` | PASS | 132.737 秒 | 29/29，`online_mask=0xff`，user-TLB 用例两轮通过 |
| `smp-b22-la64-ktest-r1-20260728` | 8 核，`KTEST=smp KREPEAT=2` | PASS | 132.830 秒 | 29/29，`online_mask=0xff`，user-TLB 用例两轮通过 |
| `smp-b22-rv64-preliminary-r1-20260728` | 8 核，`mask=0x003` | QEMU/语义门禁通过；wrapper FAIL | 296.271 秒 | raw 309/314，semantic 312/314；仅 `test_pipe` 物理行交错和两组 `kill 10`；exit 0；测试中仅文档变化触发总指纹告警 |
| `smp-b22-la64-preliminary-r1-20260728` | 8 核，`mask=0x003` | PASS | 303.789 秒 | raw/semantic 308/314；两组既有 `test_brk` 和两组 `kill 10`；无源码漂移 |

focused 结果只能证明 B22 声称的激活/IPI 基础设施以及既有 SMP 路径没有回归。初赛结果
只用于证明 CPU0 普通用户路径非回归，同样不能外推用户跨核 MM 正确性。

两轮 preliminary 均为 `configured=8`、`online_mask=0xff`，四组 basic/busybox 均有完整
START/END 和 `CC_PRELIMINARY_DONE`，进程 exit 0，无 panic、timeout 或 forbidden marker。
RV64 的 basic-musl `test_pipe` 原始块为 `cpid: 69cpid: 0`，同时包含正 PID、0、写成功和
END，故仅按既有且对基线/候选一致的块级规则恢复 3 分；raw 309 必须原样保留。

RV64 job 的 `mutation_detected=true` 来自 GPT/Codex 在 QEMU 后台运行时更新本文档集合，
并非 runner 自动修改，更不是内核源码变化。所有测试结束后再次执行：

```text
git diff --binary -- os | sha256sum
  500c1c0290bfd5c4828ce17aacfd3afe30bb5513aaed4a7e56786053aa4cec24
sha256sum os/src/mm/tlb_state.rs
  21d3e77f38b8a089d80e3043ef4f33e05ced2115459c620cd0883904d0b7af45
```

以上两份哈希是命名整理前、初赛运行时的受测源码，均与 RV64 测试前一致。命名整理后
源码哈希更新为本摘要 §1 的 `c705122d...` 与 `12231db7...`，并另行执行双架构编译；
不把整理前的 QEMU 结果伪装成整理后新鲜 QEMU。保留 wrapper `FAIL` 事实，同时接受其
整理前 QEMU/judge 证据；不为消除仅由文档造成的总指纹告警机械重跑。

DeepSeek 的验证归纳建议接受，但人工未采纳三处表述：它把文档变化猜成 runner 自动更新；
把 focused IPI 用例扩写成覆盖 MM 登记/generation；并把 LA64 基线内的 `test_brk` 失败称为
本批功能回归。最终边界以源码、原始日志和本摘要为准。

## 8. 后续 B23 验收义务

1. 用类型/所有权表达锁内 `UserTlbCommit` 生成与锁外消费，避免任一错误路径提前 drop frame。
2. 在同一个 VM 锁内串行 join mask 与 generation/目标快照。
3. 真实覆盖 unmap、mprotect、CoW、MAP_SHARED、exec 和地址空间销毁。
4. 构造 victim 无偶然 trap 全刷窗口；RV64 需要记录 trap count，LA64 保留独立强暴露证据。
5. 验证 generation race、目标在长 syscall、两个发起者交叉，以及 ack 前 frame 不复用。
6. B23 通过前继续保持用户任务 CPU0 affinity 和 `Published` fail-stop。
