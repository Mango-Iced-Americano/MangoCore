# AI 工具使用情况报告 (AI Tool Usage Report)

> Document path: `docs/00_overview/AI-Usage-Report.md`  
> Project: MangoCore  
> Coverage: 2026-04-01 to 2026-08-01
> Purpose: OS competition AI usage disclosure

## 1. 合规声明

MangoCore 项目在 2026 年 4 月至 2026 年 8 月开发期间使用了多种 AI 工具辅助代码开发、调试、架构审查、性能分析、文档生成与文档事实核查。本报告按照比赛诚信与披露要求，对已使用的 AI 工具、模型名称或平台、使用场景、产出结果、交互记录留痕和人工验证方式进行集中说明。

本项目声明：

1. 所有 AI 产出均由项目成员 Panpeach / Pan Xinyu、Pneuma 等维护者人工审查、修改、测试后才进入代码库或文档。
2. AI 工具未被授予独立提交、绕过测试、替代人工决策或隐瞒贡献来源的权限。
3. 已在相关 git commits 中保留 `Co-authored-by`、`Ultraworked with Sisyphus`、`Oracle` 等 AI 使用痕迹。
4. 已在 `docs/Work_Log.md` 中持续记录 AI 辅助分析、代码审查、根因定位和文档核查结果。
5. 本报告作为开发文档和设计文档中的独立 AI 使用披露文件，供比赛评审、答辩材料和后续归档引用。
6. 若答辩 slides 单独提交，应包含本报告末尾"答辩材料 AI 使用摘要"中的内容或等价披露。

## 2. AI 工具清单

| 工具 / Agent | 模型或版本说明 | 平台 / 来源 | 主要使用时间 | 主要用途 | 证据 |
|---|---|---|---|---|---|
| GitHub Copilot | GitHub Copilot；后端具体模型未在 commit metadata 中公开，按 GitHub Copilot 统一披露 | GitHub Copilot | 2026-04 至 2026-05 | Inline code completion、网络栈代码辅助、重构辅助 | 多个 commit 含 `Co-authored-by: Copilot <copilot@github.com>` |
| Sisyphus | Orchestrator AI；commit metadata 标识为 `Sisyphus <clio-agent@sisyphuslabs.ai>` | OhMyOpenAgent / OhMyOpenCode | 2026-05 至 2026-06 | 多步骤任务规划、并行探索、文档重构、代码修改编排、工作日志维护 | 多个 commit 含 `Ultraworked with Sisyphus` 和 `Co-authored-by: Sisyphus` |
| GPT-5.6-terra | `openai/gpt-5.6-terra` | OhMyOpenCode | 2026-07 | no_std LTP runner 诊断实现、模块拆分、构建验证与工作日志维护 | `docs/Work_Log/2026-07-17.md` |
| DeepSeek（Claude Code 兼容路由） | 本地 Claude Code CLI 对接的 DeepSeek 服务；底层精确版本未完整记录 | `cc-codex` 本地协作协议 | 2026-07 | SMP 设计只读审查、Docker/QEMU 证据归纳、独立修改建议；不授予 commit/push 权限 | `docs/Work_Log/2026-07-25.md`、`docs/Work_Log/2026-07-27.md`、对应 evidence 摘要 |
| Oracle | 高推理能力代码审查与架构咨询 agent；当前会话模型标识为 GPT-5.5 | OhMyOpenCode agent | 2026-04 至 2026-06 | 根因分析、架构评审、代码正确性验证、性能优化策略、文档事实核查 | `docs/Work_Log.md` 多处记录 `Oracle reviewed`、`Oracle analysis confirmed`、`Root cause analysis by Oracle` |
| Explore | Codebase search / pattern discovery agent | OhMyOpenCode sub-agent | 2026-05 至 2026-06 | 跨模块代码搜索、调用关系梳理、实现模式对比 | Work log 和 Sisyphus task records |
| librarian / plan / deep 等 sub-agents | 专用辅助 agents | OhMyOpenCode sub-agents | 2026-06 | 文档整理、资料检索、复杂任务拆分、局部实现检查 | Sisyphus 编排记录、文档生成 commit、Work_Log 记录 |

说明：部分 AI 平台不会在 commit metadata 中公开精确模型版本。本报告对可确认的工具名称、平台、agent 名称、commit marker 和工作日志证据进行披露；对无法从现有记录恢复的底层模型版本标注为"未完整记录"，不以猜测替代事实。

## 3. 使用时间线

| 阶段 | 时间 | 使用工具 | 使用场景 | 主要结果 |
|---|---:|---|---|---|
| 早期网络栈开发 | 2026-04-24 至 2026-05-06 | GitHub Copilot | Socket abstraction、TCP/UDP/RAW、UNIX socket、routing device、wait_io 阻塞逻辑、sendmsg/recvmsg 辅助实现 | 网络栈快速成型，commit 中保留 Copilot co-author marker |
| LTP 与文件系统问题定位 | 2026-05-19 至 2026-05-28 | Oracle, Sisyphus | LTP 0 分根因分析、RamFS PageCache、ext4 deferred unlink、VFS / mount propagation 评审 | 修复 `/dev/null ENOSYS`、MAP_SHARED SIGBUS、缺失 symlink 等关键问题 |
| VFS / PageCache / OOM 设计与审查 | 2026-05 至 2026-06 | Oracle, Sisyphus, Explore | DragonOS-style VFS 迁移、PageCache 状态机、dirty/writeback、OOM 防御、锁顺序检查 | 完成 VFS/PageCache/OOM 关键路径改造与多轮审查 |
| LTP 修复与 FS 性能优化 | 2026-06-10 至 2026-06-16 | Oracle, Sisyphus | LTP syscall 兼容性修复、FS hot path 优化、PageCache fast path、UserBuffer fast path | 修复多批 LTP 失败项，提升 lmbench/IO 性能 |
| 性能退化调试系统 | 2026-06-19 至 2026-06-20 | Oracle, Sisyphus, specialized agents | `perf_diag` counters、`drift_window`、lmbench 漂移分析、buddy allocator bitmap guard | 建立自动漂移分析脚本与诊断 counters，定位并修复 allocator 退化 |
| 后期文档系统与评审材料 | 2026-06-28 至 2026-06-30 | Sisyphus, Oracle, Explore | `Technical-Report-MangoCore.md`、`Engineering-Casebook.md`、FS/Net/MM 文档、README、评审材料事实核查 | 生成和重构大量文档，并经多轮 Oracle fact-check 修正事实错误 |
| LA64 mmap arena 边界与 trap-context 窗口修复 | 2026-07-21 | Sisyphus, Oracle | `USR_MMAP_END` 边界根因分析、固定映射相交检查、双架构 Docker/QEMU regression 事实核对 | 最终证据修正范围为 `[USR_MMAP_BASE, TRAP_CONTEXT_BASE)`，记录 RV64/LA64 TAP 1..6、LA64 `STATE=PASS STATUS=0`，并经 Oracle 最终验收 |
| Canonical normal run facade | 2026-07-22 | Sisyphus, Oracle | root/OS Makefile facade 与 dry-run contract 审查 | Oracle 发现并阻止 root logo/preflight 的重复调用；修复后在 `-j8` 下保持 validation-first、一次 setup 与 legacy `comp` 隔离 |
| 双架构 SMP idle stack | 2026-07-25 | GPT/Codex, DeepSeek | AP boot→idle 栈切换设计、ABI/内存序复核、双架构 8 核证据归纳 | AP 只在独立 idle stack 上发布 online；RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 3/3 PASS |
| SMP 调度所有权交接 | 2026-07-27 | GPT/Codex, DeepSeek | task 状态机收敛、切栈后 owner 交接与丢唤醒竞态复核 | 以六态原子状态机替代分散状态写入；双架构 4 核 SMP focused 测试均为 19/19 PASS |
| SMP 本地 TLB 提交边界 | 2026-07-27 | GPT/Codex, DeepSeek | 用户 PTE 写入收口、frame 延迟释放、LA64 ASID 边界审查和双架构 Docker/QEMU 验证 | 建立 `MmuGather` LocalOnly 协议；RV64/LA64 `CORE_NUM=1 KTEST=mm KREPEAT=2` 均为 8/8 PASS，远端 shootdown 明确 NOT RUN |
| SMP Per-CPU current 槽 | 2026-07-27 | GPT/Codex, DeepSeek | current owner 拆分、Arc/noreturn 生命周期审查、双架构 Docker/QEMU 验证 | 删除全局 PROCESSOR 与 current 裸指针；双架构 `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 19/19 PASS |
| SMP 初赛非回归门禁 | 2026-07-28 | GPT/Codex, DeepSeek | 双架构 8 核 basic+busybox 执行、judge 失败集合比较、验收规则收敛 | 发现 RV64 8 核 307/314 未达到 312 基线；建立硬条件与只升不降的失败集合门禁 |
| RV64 trap-return 半恢复现场竞态 | 2026-07-28 | GPT/Codex, DeepSeek | 用户 ELF/loader 反汇编、CSR 指令级溯源、双架构 Arc 生命周期复核与 Docker/QEMU 验证 | 统一 `SPP/SIE/SPIE` 返回契约并修复 noreturn Arc 泄漏；RV64 preliminary 312/314、LA64 SMP ktest 10/10 PASS |
| SMP AP 本地调度闭环 | 2026-07-28 | GPT/Codex, DeepSeek | scheduler-ready、AP 页表激活、远程 kernel stack 发布和双架构 8 核验证 | AP 进入本地 scheduler；定位并修复未安装 CPU-local 页表根导致的首次 dispatch 卡死；双架构 23/23 PASS |
| SMP 远程阻塞唤醒 | 2026-07-28 | GPT/Codex, DeepSeek | `last_cpu` 语义、Blocking/Blocked 竞态、批量 wake 锁序与 Docker/QEMU 验证 | AP kernel-only 任务经真实 Completion/WaitQueue 阻塞后回原 CPU；双架构 25/25 PASS |
| SMP kernel-global 撤映射与栈回收 | 2026-07-28 | GPT/Codex, DeepSeek | 全核 TLB sequence/ack、析构延迟回收、双架构 8 核 focused 与初赛回归 | 删除 AP TCB 永久保留 workaround；双架构 27/27 PASS，初赛 RV64 312/314、LA64 308/314，失败集合未扩大 |
| SMP 用户 MM 激活与 user-TLB IPI 基础设施 | 2026-07-28 | GPT/Codex, DeepSeek | VM 锁/ack 死锁审查、MM 驻留与 generation 顺序、独立 user-TLB sequence、双架构 Docker/QEMU 验证 | 保持 `Published` fail-stop，完成激活侧和全用户 IPI/ack 原语；双架构 29/29 PASS，初赛失败集合未扩大，完整 PTE shootdown 明确留给 B23 |
| SMP 用户 PTE 锁外 shootdown 与接口收敛 | 2026-07-29 | GPT/Codex, DeepSeek | VM 锁外同步、generation/ack 并发审查、frame 退休、MMU 接口重构与双架构 Docker/QEMU 验证 | 用 `AddressSpace`、`MmuGather`、`TlbFlush` 固化 `record_change -> seal -> execute`；真实 unmap 验证 ack 前不释放 frame |
| SMP RV64 页级 RFENCE 与 IPI fallback | 2026-07-29 | GPT/Codex, DeepSeek | SBI 官方 ABI、Linux/DragonOS TLB 分层对照、hart mask 映射审查与双架构 Docker/QEMU 验证 | 不增加 MM 提交类型；RV64 单页走同步 RFENCE，full/LA64 保留全用户 IPI/ack，双架构 8 核 focused 17/17 PASS |
| SMP LoongArch MM-owned ASID/epoch | 2026-07-29 | GPT/Codex, DeepSeek | 官方 CSR/INVTLB 与 Linux versioned ASID 对照、rollover 并发审查、release ELF 反汇编和双架构 Docker/QEMU 验证 | 删除 TCB ASID；定位并修复 LA64 trap-return asm 输入自覆盖；双架构 8 核 focused 19/19，初赛 RV64 312/314、LA64 308/314 |
| SMP LoongArch ASID+VPN 精准 shootdown | 2026-07-29 | GPT/Codex, DeepSeek | 官方 `invtlb 0x5`、Linux 页对粒度对照、固定原子槽并发审查和双架构 Docker/QEMU 验证 | 锁内冻结 ASID/VPN、锁外 IPI/ack；8 核并发 payload 隔离，双架构 focused 20/20 PASS |
| SMP RV64 MM-owned ASID | 2026-07-30 | GPT/Codex, DeepSeek | SATP ASID 探测、rollover 时序、SBI RFENCE FID 2 与 trap 汇编审查 | 建立 versioned ASID 与 flush-before-reuse；页级失效按 VA+ASID 执行，ASIDLEN=0 保留全刷兼容路径 |
| SMP 受控 AP 用户态闭环 | 2026-07-30 | GPT/Codex, DeepSeek | 用户 trap CPU 所有权、远程首次发布、noreturn Arc 生命周期和双架构 8 核验证 | CPU1 实际执行 getpid/yield/exit，CPU0 完成 wait/reap；普通用户调度和共享 I/O 仍未开放 |
| SMP 用户可见逻辑 CPU 查询 | 2026-07-30 | GPT/Codex, DeepSeek | Linux getcpu ABI、双架构用户探针、冻结 Docker/QEMU 验证与模型结论复核 | getcpu 迁移前后返回逻辑 CPU 0/1；双架构 focused 21/21，初赛 RV64 312/314、LA64 308/314 |
| SMP TCB affinity 调度约束 | 2026-07-30 | GPT/Codex, DeepSeek | Linux/DragonOS 数据模型对照、三条 runqueue placement 审计、冻结双架构验证 | `cpus_allowed` 约束首次发布、yield requeue 和 blocked wake；保留 CPU0 默认，运行期 affinity 未开放 |
| SMP 线程 affinity 只读 ABI | 2026-07-30 | GPT/Codex, DeepSeek | Linux raw sched_getaffinity ABI、TID/锁序审查、双架构用户探针与初赛回归 | raw syscall 返回真实 per-thread mask 与 8 字节长度；双架构 focused 21/21，初赛 RV64 312/314、LA64 308/314 |
| SMP 用户返回 RESCHEDULE 安全点 | 2026-07-30 | GPT/Codex, DeepSeek | Linux 返回用户态 need-resched 顺序、IRQ/内存序审查、IPI 驱动用户迁移与双架构冻结验证 | hard IRQ 只置位，统一任务安全点合并 timer/IPI 并最多切换一次；双架构 focused 21/21，初赛失败集合未扩大 |
| SMP 当前线程运行期 affinity | 2026-07-30 | GPT/Codex, DeepSeek | Linux/DragonOS 写侧对照、current-only 协议、冻结首错诊断与双架构回归 | `sched_setaffinity` 可让 current 从 CPU1 自迁回 CPU0；不新增状态/锁，远程 TID 保持显式未支持；双架构 focused 21/21、初赛基线不退化 |
| SMP 远程 Blocked 线程 affinity | 2026-07-30 | GPT/Codex, DeepSeek | Blocked/registry 双重所有权审计、wake 线性化、冻结只读复核与双架构回归 | 稳定 Blocked 线程可在 wake 前改 mask 并重定向到新 CPU；Running/Blocking/Queued 仍未支持；双架构 focused 22/22、初赛基线不退化 |
| SMP 远程 Queued 线程 affinity | 2026-07-30 | GPT/Codex, DeepSeek | Linux/DragonOS queued-migrating 对照、单 rq owner 交接、三轮冻结审查与双架构回归 | 稳定 Queued 线程可在不持双 rq 锁时迁移；Running/Blocking 仍未支持；双架构 focused 23/23、初赛基线不退化 |
| SMP affinity-aware 新任务放置 | 2026-07-31 | GPT/Codex, DeepSeek | 继承 mask 与首次发布冲突审计、无锁 per-CPU 负载提示、双架构 8 核冻结验证 | 新建和唤醒任务统一按 affinity/在线状态/局部性/负载选择 CPU；双架构 focused 23/23，初赛 RV64 312/314、LA64 308/314 |
| SMP 远程 Running/Blocking affinity | 2026-07-31 | GPT/Codex, DeepSeek | owner 安全点请求/完成协议、锁序反例审查、双架构 8 核冻结验证 | 远程写侧等待运行 owner 完成交接；不新增调度状态；双架构 focused 24/24，初赛 RV64 312/314、LA64 308/314 |
| SMP Per-CPU 调度 tick | 2026-07-31 | GPT/Codex, DeepSeek | 双架构 timer 官方规范、CPU0 全局 callback 边界、无 syscall 用户抢占与冻结验证 | 每 CPU 100 Hz quantum，hard IRQ 只发布 deferred 请求；双架构 focused 25/25，初赛基线不退化 |
| SMP 线程组退出与多线程 exec | 2026-07-31 | GPT/Codex, DeepSeek | Linux/DragonOS 生命周期对照、owner 自清理、clone 门禁、live ack、等待点退栈与双架构 8 核门禁 | B40 永久 group exit 与 B41 临时 exec 会话均不远程析构 Running sibling；focused 由 26/26 增至 27/27，初赛 RV64 312/314、LA64 308/314 |
| SMP trap context 与 signal 用户访存锁边界 | 2026-07-31 | GPT/Codex, DeepSeek | Linux signal ABI 对照、current owner 与 uaccess 锁序审查、冻结双架构 8 核门禁 | B45—B48 删除可逃逸 trap 引用，并让 signal frame 及状态 syscall 的用户访存位于普通锁外；初赛 RV64 312/314、LA64 308/314 |
| SMP 空闲核 work stealing | 2026-07-31 | GPT/Codex, DeepSeek | 单 runqueue owner、迁移竞态、锁外 TLB 与确定性 focused 测试审查 | 复用 `Migrating` 完成 victim→thief 交接；双架构 8 核 focused 31/31，初赛失败集合未扩大 |
| SMP Per-CPU zombie 回收 | 2026-08-01 | GPT/Codex, DeepSeek | idle 栈 Arc 寿命、跨 CPU reap 锁边界、栈映射退休与双架构验证 | 删除全局 zombie 队列，退出 CPU 在本地 idle 回收 TCB；双架构 8 核 focused 32/32，初赛基线不退化 |
| SMP 精确 active MM 驻留 | 2026-08-01 | GPT/Codex, DeepSeek | writer/enter/leave 竞态、调度切离屏障、generation 追赶与双架构冻结验证 | 历史 cached mask 收紧为精确 active mask；双架构 KREPEAT=2 focused 65/65，初赛失败集合不变 |
| SMP 有界连续 TLB shootdown | 2026-08-01 | GPT/Codex, DeepSeek | SBI/INVTLB 官方语义、固定区间槽并发审查、双架构 8 核 focused 与初赛门禁 | 不增加提交层；最多 64 页精准失效，65 页回退全刷并保持 ack 前 frame 不释放；双架构 focused 33/33 |
| SMP 真实用户访存 stale-TLB 证明 | 2026-08-01 | GPT/Codex, DeepSeek | 用户汇编 victim、真实 CoW PPN 替换、handler observed/ack 时序与假阳性排除 | 双架构 8 核 KREPEAT=2 均 67/67；精准 handler 在 ack 前推进 generation，不再由 trap-return 偶然全刷掩盖 |

## 4. 详细使用场景

### 4.1 Code Generation & Assistance

GitHub Copilot 主要用于早期代码编写时的 inline completion 和局部样板代码生成，集中出现在 2026 年 4 月下旬至 5 月上旬的网络栈开发阶段。典型范围包括：

- `Socket` abstraction 和 `File` trait 适配。
- TCP/UDP/RAW socket syscall 路径。
- `wait_io` 阻塞逻辑整理。
- `sendmsg` / `recvmsg` 等网络 syscall 辅助代码。
- UNIX socket 初始骨架和 routing device 相关实现。

代表性 commit：

- `c7f99d8e` — `跑通了netperf`
- `89272026` — `net层不做任何loop...采用waitio方法统一阻塞逻辑`
- `4ee10370` — `增加了一层socket抽象`
- `824c654d` — `初步实现了unixsocket`
- `50d97f0b` — `重新启用了routingdevice`

这些 commit 均包含 `Co-authored-by: Copilot <copilot@github.com>`。Copilot 产出仅作为代码建议，最终代码经过人工修改、编译和 QEMU 测试。

### 4.2 Code Review & Correctness Verification

Oracle 用于高风险代码变更的正确性审查、根因分析和边界条件检查。典型使用方式包括：

- 对修复方案进行事前评审，检查错误码、锁顺序、生命周期和竞态。
- 对已实现代码进行事后审查，指出遗漏边界条件。
- 对疑难 bug 进行多假设根因分析。
- 对性能优化方案进行收益/风险排序。

代表性记录：

- `2a6cb25c` commit body 明确写明：`Root cause analysis by Oracle identified three bugs causing LTP to score 0`。
- `c9399565` commit body 明确写明：`Oracle-identified issues`，列出 buddy allocator bitmap guard 的 3 个问题。
- `364bb5d6` commit body 明确写明：`Root cause identified by Oracle analysis; verified by la64 test`。
- `docs/Work_Log.md:165-265` 记录 Oracle 多轮文档事实核查，修复 judge-facing docs 中的事实不准确与绝对化表述。

### 4.3 Architecture & Design Consultation

AI 参与了若干架构设计讨论，但最终架构由项目维护者决定并实现。Oracle 和 Sisyphus 主要参与：

- VFS / MountFS 迁移到 DragonOS-style layered VFS。
- PageCache 状态机、partial-write tracking、Clock eviction、read-ahead、writeback。
- 内核 OOM 防御系统，包括 `pending_oom_kill`、`try_reserve` 和安全点 kill。
- Timer subsystem 重写，包括 overflow、one-shot、deadline semantics。
- 网络栈阻塞模型：`try_xxx`、`wait_io`、`wait_io_core` 的分层。
- Mount propagation、bind mount、`..` 跨挂载边界语义审查。

AI 在这些场景中主要产出设计建议、风险列表、实现顺序和审查意见；具体代码仍由维护者落地。

### 4.4 Performance Debugging & Optimization

AI 被广泛用于性能调试中的假设生成、计数器设计、结果解释和优化优先级排序。典型案例包括：

- `lmbench` drift detection：通过 `drift_window` 模式分窗口采集 counters。
- `scripts/analyze_drift.py`：根据 Oracle decision tree 检测 getppid cost drift、scheduler degradation、timer bloat、reclaim interference、TLB anomaly、heap growth 等异常。
- Buddy allocator bitmap guard：通过 O(1) bitmap guard 消除 `dealloc()` free-list scan drift。
- PageCache read-ahead：定位 clock eviction hole 破坏 batch 连续性导致 la64 executable page corruption。
- FS hot path optimization：`/dev/null` discard write、stat root bypass、single-page UserBuffer fast path、PageCache no-populate。
- Network stack optimization：`docs/Work_Log.md:1093-1125` 记录从 4.2 Mbps 到 144 Mbps 的 iperf TCP 34x 提升。

所有性能优化均以 QEMU、lmbench、iperf、netperf 或 focused regression tests 验证，不以 AI 推测结果作为最终结论。

### 4.5 Documentation Generation & Review

AI 参与大量文档生成、重构和事实核查工作，包括：

- `docs/Technical-Report-MangoCore.md` → 已移至 `docs/00_overview/Technical-Report-MangoCore.md`
- `docs/Engineering-Casebook.md` → 已移至 `docs/00_overview/Engineering-Casebook.md`
- `docs/03_fs/*.md`
- `docs/04_mm/*.md`
- `docs/06_net/*.md`
- `docs/README.md`
- Root `README.md`

Sisyphus 负责多文档生成和结构重排；Oracle 负责多轮事实核查，修正源码路径、架构表述、测试数据、未实现功能描述和绝对化措辞。

相关证据：

- `fd735048` — add judge-facing technical report and engineering casebook
- `81a24d2a` — apply Oracle-reviewed fixes across all docs
- `bd2ead8d` — apply Oracle-reviewed fixes to judge docs round 2
- `9b054de8` — final Oracle review fixes for judge docs
- `docs/Work_Log.md:165-265`

### 4.6 Task Orchestration & Workflow Management

Sisyphus 用于复杂任务的规划、分解和多 agent 协调。典型使用包括：

- 将大型文档系统拆分为多个模块文档。
- 编排 Explore / Oracle / specialized agents 进行并行分析。
- 组织多轮修复与验证顺序。
- 维护工作日志与经验沉淀。
- 在性能调试中生成可复用 prompt、诊断脚本和决策树。

Sisyphus 相关 commit 通常包含：

```text
Ultraworked with Sisyphus (https://github.com/code-yeongyu/oh-my-openagent)

Co-authored-by: Sisyphus <clio-agent@sisyphuslabs.ai>
```

## 5. 代表性案例

### Case 1: LTP 0 分根因分析与修复

- Evidence: `2a6cb25c`, `docs/Work_Log.md:5963-6006`
- AI tools: Oracle
- Problem: LTP 测试出现 0 分，qemu log 中缺少 Summary 输出。
- AI contribution: Oracle 识别三个独立根因：
  1. `/dev/null` 在 `O_TRUNC` redirect 时触发 `resize(0)`，返回 `ENOSYS`。
  2. `prepare_symlink()` 缺少 `ld-musl-loongarch-lp64d.so.1` 和 root-level `libtls_get_new-dtv_dso.so` symlink。
  3. LTP framework 在 `/tmp` RamFS 上执行 `mmap(MAP_SHARED)` 后 page fault，RamFS 缺少 `page_cache()`，导致 `BackingStoreFailure` 转为 SIGBUS。
- Human action: 实现 Null no-op resize、批量 symlink 创建、RamFS PageCache backend、ext4 deferred inode cleanup。
- Verification: `docs/Work_Log.md:6006` 记录 rv64 / la64 编译通过，basic test 通过，无 `/dev/null` error，无 SIGBUS。

### Case 2: PageCache read-ahead 连续性假设破裂

- Evidence: `364bb5d6`, `docs/Work_Log.md:454-456`
- AI tools: Oracle
- Problem: la64 LTP `fs_bind17.sh` 出现大量 `InstructionNonDefined`，executable pages 被错误数据覆盖。
- AI contribution: Oracle 定位到 `sync_batch_read_pages()` 跳过已缓存页后仍把非连续 pending pages 当作连续数组传给 `backend.read_pages(start, bufs)`，clock eviction 造成 `None` holes 后会把 disk page N+1 的数据写入 entry N+2。
- Human action: 将 pending pages 按连续 run 拆分，每个 run 单独调用 `read_pages(run_start, run_bufs)`。
- Verification: commit body 记录 `verified by la64 test`。

### Case 3: lmbench drift 与 buddy allocator bitmap guard

- Evidence: `4a907eb1`, `3a4bc048`, `c9399565`, `docs/Work_Log.md:717-777`, `docs/Work_Log.md:824-840`
- AI tools: Oracle, Sisyphus
- Problem: lmbench 长时间运行后出现性能漂移，怀疑 scheduler、timer、TLB、reclaim 或 heap allocator 退化。
- AI contribution:
  - Oracle 设计 drift 分析 decision tree。
  - Sisyphus 编排实现 `perf_diag` counters 和 `scripts/analyze_drift.py`。
  - Oracle 后续审查发现 bitmap guard ordering、null bitmap fallback、underflow guard 等问题。
- Human action: 实现 counters、drift window、自动分析脚本、buddy allocator bitmap guard，并修复 Oracle 指出的安全问题。
- Verification: commit `c9399565` 记录 `Build: rv64 ✅ la64 ✅`。

### Case 4: 网络栈系统性优化

- Evidence: `docs/Work_Log.md:1093-1125`, Copilot commits `c7f99d8e`, `89272026`, `4ee10370`, `824c654d`
- AI tools: GitHub Copilot, Oracle, Sisyphus
- Problem: 网络性能初期较低，iperf TCP baseline 约 4.2 Mbps。
- AI contribution:
  - Copilot 辅助早期 socket abstraction、wait_io、UNIX socket、routing device 代码生成。
  - Oracle / specialized agents 参与性能计数器设计和优化优先级分析。
  - Sisyphus 编排多轮 P0/P1/P3/E/C/A 优化。
- Human action: 实现 per-stack poll、accept waiter gating、UserBuffer 路径优化、poll 路径调整等。
- Result: `docs/Work_Log.md:1095` 记录 iperf PARALLEL_TCP 从 4.2 Mbps 提升到 144 Mbps，约 34x；netperf CRR 从 458 提升到 546，约 +19%。

### Case 5: 评审文档生成与多轮事实核查

- Evidence: `fd735048`, `81a24d2a`, `9b054de8`, `docs/Work_Log.md:165-265`
- AI tools: Sisyphus, Oracle, Explore
- Problem: 需要为比赛评审准备系统化技术报告、工程案例和模块文档，同时避免文档夸大或事实错误。
- AI contribution:
  - Sisyphus 生成和重构 judge-facing technical report、engineering casebook、README index 和模块文档。
  - Oracle 进行多轮 fact-check，指出虚构抽象、过时测试数据、源码路径错误、未实现功能误描述、绝对化措辞等问题。
- Human action: 根据 Oracle review 修改文档，移除或修正不准确内容。
- Result: 多轮文档修复 commit 保留 Sisyphus co-author marker，Work_Log 记录 Oracle 审查发现和修复项。

### Case 6: lwext4 稀疏空洞的 inode-incarnation 诊断

- Evidence: `docs/Work_Log/2026-07-17.md`
- AI tools: Oracle, GPT-5.6-terra
- Problem: 顺序运行 `gf14→gf18→gf27→gf28` 时，后续 sparse-file 用例从空洞读到稳定旧值 `0x0167`，单独运行却可通过。
- AI contribution: Oracle 结合 opt-in 逐用例 counter delta 与 PageCache registry 生命周期，定位 inode number 复用导致新文件继承旧 fully-valid 页面；随后将诊断收敛为有界 QEMU log，而非无关的 report 落盘链路。
- Verification: Docker 串行 RV64/LA64 build 通过；RV64 focused QEMU 从 1 PASS/3 FAIL 变为 4 PASS/0 FAIL。

### Case 7: LA64 mmap arena 边界与 trap-context 窗口修复

- Evidence: `docs/Work_Log/2026-07-21.md`；RED `docs/Work_Log/evidence/2026-07-21/la64-mmap-arena-red-20260721T053537+0800/`；最终 PASS `docs/Work_Log/evidence/2026-07-21/la64-mmap-boundary-final-20260721T060040+0800/`
- AI roles: Sisyphus 负责任务编排、证据整理和文档修订；Oracle 负责根因与边界审查。
- Problem: `USR_MMAP_END == TRAMPOLINE` 使半开 mmap arena 错误地覆盖 `[TRAP_CONTEXT_BASE, TRAMPOLINE)`，固定映射请求可能在 unmap 前触及 trap-context window。安全非固定 red 测试记录 `mmap accepted trap-context slot-2 hint`，即 `not ok 2 mmap_edge_cases`。
- AI contribution: 协助核对 `SIGNAL_TRAMPOLINE → TRAMPOLINE` 布局、one-based TID 槽位公式、mmap arena 半开范围和固定映射相交检查语义。
- Human action: 维护者依据源码、contracts 和 Docker/QEMU 输出将 exclusive end 修正为 `TRAP_CONTEXT_BASE`，并在普通 mmap 与 SysV shm mmap 中于 unmap 前拒绝 LA64 `MAP_FIXED`、`MAP_FIXED_NOREPLACE` 相交请求。
- Verification: RV64 → LA64 按串行顺序完成 preflight、contracts、build 和 regression；两者均为 TAP `1..6`，各有 6 个 `ok`，包含 `ok 2 mmap_edge_cases` 和 `ok 6 clone_vm_second_slot`。LA64 精确分类器为 `STATE=PASS STATUS=0`。十个源码输入 pre/post SHA-256 一致，且 source → ELF → CPIO → kernel 严格新鲜。补充证据进一步将既有 QEMU 日志绑定到真实 `/regression` ELF；Oracle 最终验收通过。该结果不外推为 full LTP 或 basic 全量覆盖。

### Case 8: Canonical normal run facade 一次性 setup 审查

- Evidence: `docs/Work_Log/2026-07-22.md`。
- AI tools: Sisyphus, Oracle。
- Problem: root generic `run` 同时把 logo/preflight 声明为 prerequisites，并在 recipe 中递归调用它们；一次 run 因而重复执行两个 setup 动作。
- AI contribution: Oracle 通过 dry-run 审查定位重复调用，并要求将一次性副作用和 `-j8` invalid-input behavior 写入 contract。
- Human action: root `run` 保留一次直接 prerequisite，移除递归 setup 调用，并以 target-scoped `.NOTPARALLEL` 保持 `validate-run → print-logo → toolchain-preflight` 顺序。
- Verification: normal-run、toolchain、source-purity、layering 与 root facade contracts 均通过；RV64/LA64 dry-run 各有一次 logo、一次 root preflight 与一次 OS dispatch；无效 `-j8` 输入无 setup 或 arch-run 输出。

### Case 9: 双架构 SMP AP idle stack 审查

- Evidence: `docs/Work_Log/2026-07-25.md`、`docs/Work_Log/evidence/2026-07-25/smp-b08-*`。
- AI roles: GPT/Codex 负责关键实现、官方 ABI 核对和最终裁决；DeepSeek 负责只读设计复核、测试证据归纳和下一工作包建议。
- Problem: AP 完成 bootstrap 后仍永久占用固件启动栈，online 无法证明 CPU 已进入稳定 idle 执行上下文。
- AI contribution: DeepSeek 独立检查双架构 naked trampoline、`tp/$r21` 保持、BSS 生命周期和 Release/Acquire 顺序，并判断现有 8 核 focused 证据已足够，不应继续机械扩测。
- Human action: 维护者拒绝了把可写 stack 改成 immutable static、以及把 timer/runqueue/MM 同时塞入下一包的过宽建议；保留 `static mut + addr_of!`，并把后续范围收敛为最小 IPI mailbox/ack。
- Verification: RV64 实际以 hardware hart6 冷启动、LA64 以 CPU0 冷启动，两者均达到 `online_mask=0xff`、SMP ktest 3/3 PASS；ELF 反汇编确认切栈指令与页对齐 BSS 符号。

### Case 10: SMP 调度所有权与阻塞唤醒交接

- Evidence: `docs/Work_Log/2026-07-27.md`、`docs/Work_Log/evidence/2026-07-27/smp-b15-summary.md`。
- AI roles: GPT/Codex 负责状态机取舍、实现与最终验收；DeepSeek 负责冻结源码的只读竞态审查和 Docker/QEMU 结果归纳。
- Problem: 通用 `TaskManager::add()` 若能把仍在当前 CPU 内核栈上执行的任务直接改成 queued，真正多核后另一 CPU 可能在 context switch 完成前取走同一 TCB；阻塞登记与切栈之间还存在提前 wake 窗口。
- AI contribution: DeepSeek 复核了六态方案、CAS 内存序、interruptible registry 与 current slot 的短暂重叠，并指出 nice-aware 选择仍在全局调度锁内读取 `task.inner` 的后续锁序债务；未建议继续扩张瞬态状态。
- Human action: 删除通用调度 add 入口，以 `publish_task()`、`fetch_task(cpu)` 和 idle 侧 `finish_switch_out()` 收口 owner 交接；仅保留必要的 `Blocking(cpu)` 瞬态，并由统一 wake CAS 区分提前取消阻塞与真正重新入队。
- Verification: RV64、LA64 `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 19/19 PASS；双架构 normal kernel build 通过，RV64 WaitQueue focused 测试为 4/4 PASS。证据不外推为 AP 用户任务调度、迁移或远程 TLB 正确性。

### Case 11: SMP 用户 PTE 的本地 TLB 提交边界

- Evidence: `docs/Work_Log/2026-07-27.md`、`docs/Work_Log/evidence/2026-07-27/smp-b16-summary.md`。
- AI roles: GPT/Codex 负责架构契约核对、实现、审查裁决与证据边界；DeepSeek 负责实施前生命周期审计、冻结 diff 只读审查和受限 Docker recipe 结果归纳。
- Problem: 用户 PTE 修改与 TLB 刷新分散在 VMA、缺页、CoW、OOM 和退出路径，无法统一表达“先失效旧翻译，后释放/复用物理页”，也没有可供后续远端 shootdown 接入的提交边界。
- AI contribution: DeepSeek 的前置审计指出旧 unmap 顺序的 frame 生命周期风险；冻结审查无 P0/P1，并发现 LA64 旧安全接口仍使用当前 ASID 精确失效的潜在误用点。
- Human action: 建立 `MmuGather` 和 `Unpublished/LocalOnly/Published` 三态发布边界，收口所有用户 PTE 写入，将失效映射的 frame 延迟到本地 flush 后释放；采纳 LA64 审查项，但拒绝把释放构建的生命周期断言降为 `debug_assert!`。
- Verification: RV64、LA64 `CORE_NUM=1 KTEST=mm KREPEAT=2` 严格串行，均为 8/8 PASS，受测源码指纹前后一致。该证据只验收 CPU0 LocalOnly 路径；远端 generation/ack、MM-owned ASID 和 kernel-global shootdown 均未运行。

### Case 12: SMP Per-CPU current 所有权与 Arc 生命周期

- Evidence: `docs/Work_Log/2026-07-27.md`。
- AI roles: GPT/Codex 负责所有权设计、代码实现、生命周期复核与最终裁决；DeepSeek
  负责冻结源码的只读审查、受限 Docker recipe 执行和结果归纳。
- Problem: 全局 `PROCESSOR`、current 裸指针和伪造 `'static` 引用无法扩展到多个
  scheduler CPU；简单改成 `Arc` 后，退出与 trap noreturn 路径又可能因为旧 Rust
  栈帧不展开而泄漏引用。
- AI contribution: DeepSeek 首轮构建准确归纳了双架构一致的 22 个迁移错误，但它
  提议恢复引用适配层。维护者拒绝该建议并逐点显式借用，随后通过人工控制流审查
  发现了编译器和初轮报告均未指出的 noreturn Arc 泄漏风险。
- Human action: 将 current/idle 状态嵌入每个 `PerCpu`，删除裸指针和可变身份影子
  cache，规定 `task.inner -> local processor` 的 dispatch 顺序，并在所有不返回边界
  前显式释放本地 current `Arc`。
- Verification: 双架构 normal kernel build 通过；RV64/LA64
  `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 19/19 PASS，四个 recipe 无源码漂移。
  该结果不外推 per-CPU runqueue、远程 enqueue 或普通用户任务跨核运行。

### Case 13: SMP 双架构 8 核初赛非回归门禁

- Evidence: `docs/Work_Log/2026-07-28.md`；原始 prompt、模型输出和 child job 日志
  保留在本地忽略的 `cc-codex/`，不上传仓库。
- AI roles: DeepSeek 负责受限 Docker recipe 执行、完整日志初审和失败集合整理；
  GPT/Codex 独立核对源码指纹、串口标记、judge JSON，并裁决正式验收规则。
- Problem: QEMU 正常退出、四个组脚本退出 0 并不表示 judge 无回退；只比较总分还会漏掉
  “同分但失败项换位”。拓扑不匹配的 required marker 也可能让 runner 状态与内核事实不同。
- AI contribution: DeepSeek 识别出 B17 RV64 8 核新增的 musl `test_fstat` 和
  `test_write` 失分，并按要求补做单核判别。它将一次对照推断为确定 SMP 根因、且把
  固定 `online_mask=0xff` 导致的 child FAIL 误写为 recipe PASS；这些结论经人工复核后
  均未进入正式判定。
- Human action: 将门禁拆为启动/marker/退出/源码指纹硬条件与 judge 递增基线；失败按
  group/test 身份集合与逐项 pass 下限比较，改善需稳定证据和人工确认后才能 ratchet，
  退化不得降低基线。门禁仅在用户路径 T3 节点和阶段/合并候选触发，避免纯文档重复运行。
- Verification: 同一冻结 HEAD `bafe04ad` 上，RV64 8 核为 307/314，硬条件通过但
  非回归失败；LA64 8 核为 308/314，失败集合相对 305 基线缩小；RV64 单核为
  312/314，未复现两项新增失分。当前证据不能在“8 核相关问题”和“单次波动”之间定因。

### Case 14: RV64 trap-return 半恢复现场竞态

- Evidence: `docs/Work_Log/2026-07-28.md`；DeepSeek 原始任务、输出和 Docker child
  日志仅保留在本地忽略的 `cc-codex/`，不上传 GitHub。
- AI roles: GPT/Codex 负责镜像/ELF/汇编/CSR 的指令级溯源、修复与最终裁决；DeepSeek
  负责独立重复实验、聚焦源码复核和受限 Docker 验证。
- Problem: RV64 8 核 preliminary 偶发在用户动态加载器 `0x80011c5c` 首条栈保存处
  fault，用户 `sp` 精确变成 trap-context VA；同时 owned current-task 改造使 syscall
  分支在 noreturn 返回边界新增一个 TCB `Arc` 泄漏风险。
- AI contribution: DeepSeek 的一次 8 核重复运行证明任意非零 boot hart 并非必现，
  但早期将用户虚拟地址误判为 OpenSBI 物理地址、两次只读审查超时，均未被人工采纳。
  修复后它独立确认统一返回态和双架构 Arc 生命周期，并归纳 RV64 preliminary 与 LA64
  SMP ktest 结果。
- Human action: 维护者依据相同用户二进制、动态加载器反汇编、slot-1 地址和 RISC-V
  `SIE/SPIE/SPP` 语义，确认 `csrw sstatus` 后的半恢复窗口可被 timer 打断；统一返回态为
  `SPP=User、SIE=0、SPIE=1`，并在双架构 syscall noreturn 边界显式释放临时 Arc。
- Verification: RV64 `CORE_NUM=8` preliminary 为 312/314，`fstat/write` 全部恢复；
  LA64 `CORE_NUM=8 KTEST=smp` 为 10/10 PASS。单次 RV64 PASS 只作回归烟测，竞态关闭
  主要由返回态不变量和官方 CSR 语义证明。

### Case 15: SMP Per-CPU RunQueue 容器拆分

- Evidence: `docs/Work_Log/2026-07-28.md`、
  `docs/Work_Log/evidence/2026-07-28/smp-b18-runqueue-summary.md`；DeepSeek 原始任务与
  Docker 日志仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责锁序设计、实现和证据边界裁决；DeepSeek 负责冻结源码只读
  复核、双架构 Docker build/QEMU 执行和日志归纳。
- Problem: runnable 任务仍集中在全局 ready queue，既无法表达物理队列 owner，也让
  后续远程 enqueue/负载选择只能继续扩大单一全局锁。
- AI contribution: DeepSeek 确认旧 ready queue 生产调用点已被移除、
  `TASK_MANAGER -> 单个 RunQueue` 锁序闭合，并执行四项串行门禁。其报告把
  `nr_running` 与锁内长度描述成已精确逐点验证，人工复核测试源码后将该结论收敛为
  “当前生产路径上的间接非回归证据”。
- Human action: 每个 `CpuTaskState` 增加独立 RunQueue 和排队数快照，以原子
  nice/vruntime hint 消除 `task.inner` 嵌套；生产 target 继续固定 CPU0，未提前引入
  AP 调度、迁移或 work stealing。
- Verification: RV64/LA64 `CORE_NUM=8` kernel build 均通过；双架构
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 19/19 PASS。补充执行的 `mask=0x003` 门禁中，
  RV64 raw/semantic 均为 312/314；LA64 raw 为 302/314。后续反汇编证明官方
  `test_pipe` 的 `printf` 会把一个 cpid 逻辑行拆成多个 write syscall，两个失败块也都
  保留了 0/正 PID 与 pipe write-success 证据。GPT/Codex 据此拒绝无效的 TTY 行锁修正，
  并把 §8.2 改为 raw/semantic 双账本；B16 与 B18 使用同一归一化规则后，LA64 semantic
  均为 308/314；干净 B17 对照也以 raw 305/semantic 308 复现 glibc 片段交错。
  DeepSeek 第一轮错误的 syscall 中途抢占推断未被采纳，第二轮按完整 safe-point 调用链
  复核后同意撤回该建议。

### Case 16: SMP AP 本地调度与 kernel stack 发布

- Evidence: `docs/Work_Log/2026-07-28.md`、
  `docs/Work_Log/evidence/2026-07-28/smp-b19-ap-scheduler-summary.md`；原始 DeepSeek
  输出和 Docker job 只保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责并发协议、实现、官方架构规范核对和最终裁决；DeepSeek
  负责失败日志的独立只读溯源，以及串行 Docker/QEMU 执行。
- Problem: Per-CPU RunQueue 已存在，但 AP 没有 scheduler-ready 屏障和本地调度循环；
  首轮远程任务实验还使全部 AP 在首次 context switch 后静默卡死。
- AI contribution: DeepSeek 从“首个远程用例失败、后续所有 IPI/STOP 级联失败”定位到
  AP 从未安装 CPU-local kernel page-table root。早期 IPI 仅访问恒等映射区，不能证明
  高虚拟地址 kernel stack 可用。它建议在 AP 进入 scheduler 前 activate；该结论经人工
  调用链复核后采纳。
- Human action: 增加 scheduler-ready/entered 屏障和 AP 精简 scheduler；将 ktest entry
  下沉为 TCB 不可变字段；在 AP activate 之外再实现带 sequence/ack 的目标 TLB sync，
  确保动态 stack 映射先可见、后入队。拒绝仅依赖“AP 冷 TLB”的偶然性，也未提前开放
  用户任务迁移、共享子系统或通用 shootdown。
- Verification: 首轮 RV64 为 16/23 RED；修复后 RV64、LA64
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 23/23 PASS，包含两轮 AP scheduler/remote
  exactly-once 和 terminal STOP，受测源码 before/after 指纹一致。

### Case 17: SMP 远程 blocked wake 与锁外 IPI 发布

- Evidence: `docs/Work_Log/2026-07-28.md`、
  `docs/Work_Log/evidence/2026-07-28/smp-b20-remote-wake-summary.md`；原始 DeepSeek
  请求、模型输出和 Docker job 仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责状态机最小化、锁序/内存序设计、实现与报告裁决；DeepSeek
  负责前置只读反例审查，并按自然语言任务驱动 allowlist Docker runner 串行验证。
- Problem: `Blocked` 不携带最近运行位置，统一 wake 硬编码 CPU0；即使任务进入远端
  runqueue，也缺少在释放调度锁后聚合发送 `RESCHEDULE` 的生产交接。
- Human action: 不新增状态，只增加非 owner 的 `last_cpu` 提示；在
  `TASK_MANAGER -> 单个 RunQueue` 下唯一提交 `Blocked -> Queued(target)`，锁外再发送
  doorbell。人工拒绝 DeepSeek 对 relaxed 内存序和 WaitQueue 外围锁的过度推断，采纳
  显式 Release/Acquire 与排除 STOP CPU 的防御建议。
- Verification: RV64/LA64 8 核 normal build 均 PASS；两架构
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 25/25 PASS。每轮 7 个 AP 任务经真实
  Completion/WaitQueue 阻塞后回原 CPU，terminal STOP 通过，受测源码无 mutation。

### Case 18: SMP kernel-global 撤映射与内核栈延迟回收

- Evidence: `docs/Work_Log/2026-07-28.md`、
  `docs/Work_Log/evidence/2026-07-28/smp-b21-kernel-mapping-retirement-summary.md`；原始
  DeepSeek 请求、模型输出和 Docker/QEMU 日志仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责协议设计、源码实现、并发推理、失败归因与最终裁决；DeepSeek
  负责冻结源码只读审查、串行 Docker 验证、初赛日志计分和 failure multiset 独立复核。
- Problem: 动态 kernel stack 只能在目标 CPU 使用前同步“新增映射”，TCB 析构时却仍会
  本地撤映射并立即释放 frame/slot。远端 CPU 可能保留旧 TLB，且 `Drop` 不能安全地持 MM
  锁等待 IPI ack，因此 B19/B20 只能永久保留 AP 测试 TCB。
- Human action: 将撤映射拆为“锁内清 PTE 并保留 frame → 锁外全核 flush/ack → 释放 frame
  与 slot”；`KernelStack::drop` 只向固定容量、无堆退休队列提交 slot，CPU0 idle 安全点再
  批量回收。handler 固定为 request snapshot → full invalidate → Release ack，并区分
  publish 不接受 STOP 与 unmap 可接受 STOP 的语义。
- AI adjudication: 采纳 DeepSeek 关于 STOP race、等待时 IRQ 可达性和 LA64 global TLB
  失效范围的风险提示；拒绝在 MM 同步层直接执行 deferred timer callback，因为这会把
  timer/scheduler 安全点反向耦合进 MM。也拒绝把首轮 `AreaNotFound` 归因于重复入队，真实
  根因是把字节地址直接转成 VPN；最终审查中“init ELF 清理发生于 AP 上线前”的描述也与
  当前启动顺序不符，不作为证明。
- Verification: RV64/LA64 normal kernel build 均 PASS；两架构
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 27/27 PASS。新用例连续两轮各创建 129 个 AP
  kernel-only 任务，真实溢出 128 项 stack cache，验证全部 AP ack、TCB 析构、frame/slot
  回收及同 VA 再映射。初赛回归为 RV64 312/314、LA64 308/314，均精确匹配既有允许失败
  集合；它只证明 8 核 online 与 CPU0 普通用户路径未退化，不证明用户 MM、FS 或网络跨核。

### Case 19: SMP 用户 MM 激活与 user-TLB IPI 基础设施

- Evidence: `docs/Work_Log/2026-07-28.md`、
  `docs/Work_Log/evidence/2026-07-28/smp-b22-user-tlb-foundation-summary.md`；原始
  DeepSeek 审查、Docker/QEMU 日志与任务状态只保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责源码调用链审计、两阶段协议设计、实现、内存序裁决和证据验收；
  DeepSeek 负责冻结源码只读设计/最终审查、后台串行 Docker 测试与结果独立归纳。
- Problem: B16 的 `MmuGather` 只有 LocalOnly 语义；直接在其 `commit()` 内加入远端等待会
  持有进程 VM 锁。目标 CPU 可能在 IRQ-off page fault 中等待同一锁，于是形成持锁等 ack
  与目标等锁的死锁。用户 trap-return 也尚未登记哪些 CPU 可能缓存该 MM。
- Human action: 先建立每 MM 的单调 cached CPU mask、generation/observed 和 trap-return
  激活入口；另建独立 user-TLB request/ack 与锁外全用户失效原语。第二颗 CPU 登记后仍把
  MM 标为 `Published` 并 fail-stop，不在两阶段提交完成前开放 PTE 写入或用户迁移。
- AI adjudication: 采纳 DeepSeek 对 VM 锁死锁、join-before-generation、独立 sequence 和
  全量失效的建议；把它提出的跨 Atomic 顺序风险记录为 B23 证明义务，但不采纳“只把
  generation 改成 AcqRel fetch_add 即可”的简化，因为真正串行边界是激活与修改方共用的
  VM 锁。LA64 当前 ASID 仍归 TCB，故采用 `invtlb 0x3` 全 non-global 失效而非伪造 MM ASID。
- Verification: RV64/LA64 normal kernel build 均 PASS；两架构
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 29/29 PASS，新生产原语的两轮 IPI/ack 用例通过。
  初赛 RV64 raw 309/semantic 312（`test_pipe` 物理行交错 + 两组 `kill 10`），LA64
  raw/semantic 308（两组既有 `test_brk` + 两组 `kill 10`），失败集合未扩大。RV64 wrapper
  因 GPT/Codex 并行更新文档而 fail-closed；人工复核生产源码哈希未变、QEMU exit 0 后接受
  测试证据，但不改写 wrapper FAIL，也不为机械绿灯重跑。
  测试没有修改真实用户 PTE，因此 generation race、stale translation、ack 前 frame
  不复用、MM-owned ASID 和用户跨核执行均明确为 NOT RUN。

### Case 20: SMP 用户 PTE 两阶段 shootdown 与 frame 退休

- Evidence: `docs/Work_Log/2026-07-29.md`、
  `docs/Work_Log/evidence/2026-07-29/smp-b23-user-tlb-flush-summary.md`；原始 DeepSeek 审查和
  Worker 日志仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计并实现 `AddressSpace`、两阶段提交、锁序调整和最终证据
  裁决；DeepSeek 只读复核 generation/observed、request/ack 合并、frame 生命周期与
  trap-return 性能边界。机械构建/QEMU 由本地 Docker Worker 严格串行。
- Problem: 真实 PTE 修改原先在外层 VM guard 持有期内提交。如果在该位置增加远端等待，
  目标 CPU 可能正在 IRQ-off page fault 中等待同一 VM 锁；同时 unmap/zombie 若在 ack 前
  drop 数据页或页表页，会把 stale translation/page walk 变成物理页 UAF。
- Human action: 用不暴露可变 guard 的 `AddressSpace::write()` 包住所有进程 VM
  修改；把外层/锁内数据明确为 `AddressSpace`/`AddressSpaceInner`，删除 pending、
  publication 和多层 commit 原型。`UserMapper` 写 PTE，唯一 `MmuGather` 记录范围与
  frame，`seal()` 生成锁外 `TlbFlush`，`execute()` 才失效本地 TLB、广播 IPI、等待
  ack、推进 observed 并释放 frame。另外修正
  trap/clone/OOM/SysV SHM 锁序，并让迟到且已被 ack 覆盖的 user-TLB reason 不再重复全刷。
- AI adjudication: 采纳 DeepSeek 对 VM 锁边界、单次 generation 和 ack 后 observed 闭环的
  核对；纠正其“未 Acquire 新 generation 就不受 PTE 影响”和“不同 MM 共享 observed”的
  表述，最终正确性依赖共同 VM 锁、handler 的 request-before-flush-before-ack 与 generation
  重查。也不接受“全刷在 QEMU 上不可测”的无数据推断；当前仅将其视为正确性基线。
- Verification: 重构期间两架构诊断 build 虽退出 0，但因源码同时变化被 runner
  fail-closed，不计入验收。最终冻结源码的 RV64/LA64 8 核 SMP focused 均为
  16/16 PASS；新用例对共享页表执行 unmap，确认 request 已发布但未 ack 时 frame
  计数不变，ack 后才增加一页。`mask=0x003` 为 RV64 312/314、LA64 308/314，
  失败集合没有扩大；四项均退出 0、无 timeout/forbidden marker，且源码指纹不变。

### Case 21: SMP RV64 页级 RFENCE 与软件 IPI fallback

- Evidence: `docs/Work_Log/2026-07-29.md`、
  `docs/Work_Log/evidence/2026-07-29/smp-b24-rfence-summary.md`；模型原始分析与 Docker
  子任务日志仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 SBI 官方规范、Linux RISC-V `flush_tlb_range` 和 DragonOS
  `MmuGather/TLB` 分层，负责接口设计、实现、并发裁决和证据边界；DeepSeek 自主选择双架构
  build/focused ktest，读取日志并复核 ABI、fallback 与 frame 生命周期。
- Problem: B23 已能区分 `Page/Full`，但只要目标含远端 CPU 就丢弃该信息并全刷用户 TLB。
  直接给软件 IPI 增加单个共享 range payload 会在多个发起者并发时发生覆盖或错误合并。
- Human action: 保持 `MmuGather -> TlbFlush` 主链不变；SMP 同步层仅接受可选单页提示。
  RV64 把逻辑 CPU mask 转成物理 hart mask，调用同步 SBI `REMOTE_SFENCE_VMA`；固件缺失时
  明确回退已有全用户 IPI/ack。LA64 与 full flush 继续走该 fallback，不新增 slot 或 commit。
- AI adjudication: 接受 DeepSeek 对四项门禁均 PASS 的事实，但纠正它把“全 8 核 mask +
  boot hart=4”描述为动态证明逆映射的过度结论；全量 mask 恰好仍为 `0xff`，最终测试改为
  逻辑 CPU0/1 子集后再冻结。没有采纳立即实现 LA64 range slot 或 ASID，因为二者需要独立
  生命周期和并发证明。
- Verification: 初轮 RV64/LA64 normal build 与 8 核 SMP focused 均 PASS。最终把测试
  收紧为逻辑 CPU0/1 后重新冻结：RV64 boot hart=5，逻辑 `0b11` 转为物理 `0b100001`，
  17/17 PASS 且页级用例不增加 software request；LA64 17/17 PASS 并增加 request/ack。
  两项均 exit 0、无 timeout/forbidden marker、源码指纹不变；双页 Full 退休窗口均通过。

### Case 22: SMP LoongArch MM-owned ASID 与 epoch rollover

- Evidence: `docs/Work_Log/2026-07-29.md`、
  `docs/Work_Log/evidence/2026-07-29/smp-b25-la64-asid-summary.md`；DeepSeek prompt、模型输出与
  完整 child logs 按协作边界只保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 查阅 LoongArch 官方手册与 Linux LoongArch `mmu_context`，负责所有权
  模型、rollover 并发协议、实现、release ELF 指令级定因和最终裁决；DeepSeek 在只读冻结
  工作树上执行 build、8 核 focused/初赛，汇总动态证据并指出 IRQ 配对与 VM 锁边界。
- Problem: 旧 ASID 由 TCB 分配并在 drop 时立即回收，同一共享 MM 的线程可能使用不同
  标签，且其它 CPU 的 stale translation 尚未清除时编号即可复用。恢复汇编只能靠每次
  context switch 全量 `invtlb` 兜底。
- Human action: 把 versioned `asid_context` 放进每 MM 的 `TlbContext`；同一 epoch 内编号
  单调分配，耗尽时发布 rollover gate、同步全 CPU user-TLB flush/ack、最后推进 epoch。
  `UserVmContext` 只承担一次用户返回所需的页表根/ASID快照，没有增加第二套 commit 链。
- AI adjudication: 独立复核 leader/waiter 的中断恢复和
  `VM unlock -> rollover -> retry` 锁序，以及 IRQ-off trap-return 窗口与远端 ack 的先后关系。
  首轮 LA64 初赛在用户态入口 RED 后，没有恢复每次 context switch 全刷；最终 ELF 反汇编
  证明泛型 `in(reg)` 与连续 `move` 发生输入自覆盖，随后改为显式 `$a0/$a1/$a2` ABI 约束。
  DeepSeek 把一次 wrapper mutation 误归因于构建产物，Codex 根据时间和 diff 指纹纠正为
  自身并行编辑 tracked workflow 文档，并拒绝把该轮作为冻结证据。
- Verification: RV64/LA64 normal build 均 exit 0；双架构 `CORE_NUM=8 KTEST=smp` 均
  19/19 PASS。LA64 动态读取 `ASIDBITS=10`，跨 CPU 共享 MM 使用同一非零 ASID，自然耗尽
  1023 个用户编号后恰好发生一次 rollover，并观测到远端 request 增加。真实用户路径回归
  为 RV64 312/314、LA64 308/314，四组完整且失败集合未扩大；修复后的 LA64 日志不再出现
  `PageInvalidStore/PageInvalidFetch`、panic 或 timeout。

### Case 23: SMP LoongArch ASID+VPN 精准 shootdown

- Evidence: `docs/Work_Log/2026-07-29.md`、
  `docs/Work_Log/evidence/2026-07-29/smp-b26-precise-shootdown-summary.md`；DeepSeek prompt、
  完整输出和原始 Docker 日志仍只保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 LoongArch 官方 `INVTLB` 与 Linux LoongArch 页级刷新实现，负责
  slot 生命周期、内存序、超时 fail-stop、实现和最终裁决；DeepSeek 只读审查冻结 diff，
  自主执行串行双架构 build/8 核 focused，并归纳锁序、frame 退休和动态结果。
- Problem: B25 已有 MM-owned ASID，但单页 PTE 修改在 LA64 仍清除全部 non-global TLB；
  若只增加一个全局 ASID/VPN payload，多个 CPU 同时发起时又会因 reason bit 合并而覆盖。
- Human action: 在既有 `MmuGather::seal()` 内冻结 ASID 与 VPN；每个发起 CPU 使用一个固定
  原子 slot，IPI handler 扫描全部 slot，执行 `invtlb 0x5` 后才 ack。timeout 时不复用槽，
  避免迟到 doorbell 形成 ABA；退休 frame 继续由唯一 `TlbFlush` 在 ack 后释放。
- AI adjudication: 采纳 DeepSeek 对 VM 锁外等待、无锁 handler 和 STOP 分支的审查；纠正其
  将并发用例概括为“不同 ASID/VPN”的表述——实际为同一 MM/ASID 下不同 VPN，验证 slot
  payload 隔离。ASID 跨 CPU 所有权与 rollover 由独立测试覆盖。LA64 的“精准”也限定为
  ASID + 硬件相邻页对，不声称只失效一个 4 KiB 页。
- Verification: RV64、LA64 normal build 串行 exit 0；两架构
  `CORE_NUM=8 KTEST=smp KREPEAT=1` 均为 20/20 PASS，online mask `0xff`，页级后端与
  8 核并发 slot 用例均通过，full-request 计数不增长。四项无 timeout/forbidden marker，
  冻结源码前后指纹一致。该证据不覆盖连续 range、RV64 MM-owned ASID 或普通用户迁移。

### Case 24: SMP RISC-V MM-owned ASID 与按 ASID 页级 RFENCE

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b27-rv64-asid-summary.md`；DeepSeek 的 prompt、
  原始输出和 Docker 日志仍只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 RISC-V privileged specification、SBI RFENCE specification 与
  Linux RISC-V ASID/TLB 实现，负责 allocator、trap 交接时序、实现和最终裁决；DeepSeek
  只读审查设计并归纳串行双架构 Docker/QEMU 结果，不拥有 commit/push 权限。
- Problem: RV64 之前固定使用 ASID 0，用户/内核 SATP 切换必须全量 `sfence.vma`；页级
  RFENCE 也无法限定到目标 MM。直接增加 allocator 仍不够：rollover ack 后若 trap-return
  重新安装提前读取的旧 SATP，旧 epoch 仍可能再次运行。
- Human action: BSP 通过 SATP WARL 语义探测 ASIDLEN；每 MM 的既有 `TlbContext` 保存
  versioned ASID，同一 epoch 内不复用，耗尽后先完成全 CPU flush/ack。用户 SATP 编入
  MM-owned ASID，本地页失效使用 `sfence.vma va, asid`，远端使用 SBI RFENCE FID 2；
  `trap_return` 到 `sret` 保持 IRQ-off，关闭 ack 后重装旧 context 的竞态。
- AI adjudication: 采纳 DeepSeek 对 FID 2 第五参数、SATP 编码和公共 `asid_context` 的
  审查；补充其未指出的旧快照竞态，并保留 ASIDLEN=0 的 switch-time 全刷路径。没有为
  RV64 新增第二套 MM commit 类型，也没有把测试计数器塞入生产对象。
- Verification: 最终源码的 RV64 8 核 SMP focused 为 20/20 PASS；双架构 preliminary
  recipe 均完成 normal build 与四组 `mask=0x003`，RV64 312/314、LA64 308/314，失败
  集合未扩大或换位。最终 ELF 确认条件式 trap fence、双操作数 `sfence.vma` 和 FID 2
  的 a4 ASID。仓库 lint baseline 漂移，且脚本遗留临时 stub，因此仓库级状态如实记为
  `partial`，未用 capture-baseline 掩盖 RED。

### Case 25: SMP 受控 AP 用户态 syscall/退出闭环

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b28-ap-user-summary.md`；设计审查、模型报告和原始
  Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计并实现远程首次发布、trap owner 校验、用户探针与生命周期收口，
  独立核对源码哈希和日志；DeepSeek 先只读审查最小边界，再通过受限 Docker gateway
  串行执行双架构 focused/初赛，并复核最终收敛 diff。提交仍由 GPT 在用户批准后执行。
- Problem: B27 已完成双架构 ASID/TLB 基础设施，但用户 trap handler 仍硬编码 CPU0，
  也没有真实用户任务证明 AP 的页表/ASID、CPU-local 寄存器、yield 恢复和退出回收能连成
  闭环。进一步审查还发现，若 trap handler 的临时 TCB `Arc` 跨过非返回 exit syscall，
  Rust 栈帧永不析构会导致 TCB 与内核栈永久存活。
- Human action: 抽出唯一 `publish_task_on()`，固定“内核栈映射同步 → runqueue 发布 →
  锁外 doorbell”；以 `current_trap_task()` 一次取得 current 并验证 `Running(cpu)`，返回型
  syscall 后重新读取 CPU，非返回 syscall 前主动 drop Arc。ktest 内嵌两架构最小用户指令，
  在 RW 匿名页装载后经正式 mprotect 收紧为 RX；CPU1 执行 getpid、yield 和 exit，CPU0
  验证 zombie、wait/reap 与 Weak 释放。
- AI adjudication: 没有采纳 DeepSeek 增加独立 `/test_ap_user` ELF 和测试专用生产发布 API
  的建议，因为会扩大构建面并复制协议；也纠正了“exit 完成第三次往返”和“B28 已动态证明
  generation race/远端 PTE shootdown”的过度表述。正确边界是两次返回用户态、一次非返回
  exit trap，以及 CPU0 创建/AP 执行/CPU0 回收，而非同一任务跨核迁移。
- Verification: 最终代码指纹下 RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 21/21 PASS，
  `smp::ap_user_syscall_round_trip` 明确为第 20 项且通过，online mask 均为 `0xff`。功能实现
  首轮的双架构 `mask=0x003` 仍为 RV64 312/314、LA64 308/314，失败集合未扩大；最终只做
  owner helper、RW→RX 和测试确定性收敛，按风险不重复整套初赛。仓库 lint 继续因既有
  baseline 漂移而 RED，未刷新基线。

### Case 26: SMP 用户任务显式 yield 安全点迁移

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b29-yield-migration-summary.md`；原始 DeepSeek prompt、
  报告和 Docker/QEMU 日志继续只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计一次性迁移请求、调度所有权交接和 focused 用例，并独立裁决日志；
  DeepSeek 负责冻结 patch 只读审查、双架构 focused/初赛执行和结果归纳。提交仍由 GPT 在用户
  明确批准后执行。
- Problem: B28 的用户探针从第一次 dispatch 起就在 CPU1，不能证明同一内核栈、trap frame
  和 MM 在 syscall 内 yield 后可跨 CPU 恢复。直接增加 `Migrating` 状态或同时锁两个 runqueue
  会扩大状态机并破坏既有锁序。
- Human action: TCB 增加不具有 owner 语义的一次性 `migration_target`；请求先同步目标内核栈
  TLB，再 Release 发布。源任务真正切回 idle 栈并清空 current 后，只锁目标 runqueue 完成
  `Running(source) -> Queued(target)`，锁外发送 IPI。Blocking/Zombie 丢弃未消费请求，未把
  本节点扩成完整 affinity。
- AI adjudication: DeepSeek 的冻结 diff 审查没有阻断问题；GPT/Codex 接受内存序收紧，拒绝
  跳过 kernel-stack TLB 同步。首轮测试报告误称 panic 发生在构造期；根据 shootdown 等待集合
  排除了 current，而 missing CPU 为 0，GPT/Codex 反推出发起者是 CPU1，定位为迁移后退出时
  CPU0 runner 关中断无法 ack。最终双架构 PASS 验证该判断。
- Verification: 最终 RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 21/21 PASS，
  `smp::user_task_migrates_on_yield` 明确通过，online mask 均为 `0xff`。同一生产 diff 的
  `mask=0x003` 初赛分别为 312/314、308/314，四组 END 完整、失败集合未扩大。没有把模型
  文本、进程退出 0 或总用例数单独当作 PASS。

### Case 27: SMP `getcpu()` 真实逻辑 CPU

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b30-getcpu-summary.md`；原始 prompt、只读报告和四份
  Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux ABI 设计逻辑 CPU 快照和双架构反假通过探针，并独立读取
  child result、TAP 与 judge JSON；DeepSeek 负责冻结 patch 只读审查、串行执行双架构
  focused/初赛门禁和提供汇总。DeepSeek 不修改源码、不提交、不 push。
- Problem: 旧 `getcpu()` 永远写 0，即使 B29 已把同一用户任务迁到 CPU1，用户态也无法观察
  真实 CPU。直接开始 affinity 会让普通任务过早进入尚未审计的 FS/net/driver AP 路径。
- Human action: syscall 只快照一次 `smp::cpu_id()`，返回 scheduler 使用的连续逻辑编号；无
  NUMA 时 node 保持 0。B29 探针在真实 yield 前后分别断言 0/1，任何 syscall 错误、固定 0、
  未迁移或错误起跑都会 exit(1)。没有修改 runqueue、TCB 或默认 CPU0 发布策略。
- AI adjudication: 采纳 DeepSeek 的 yield 返回值显式检查；纠正其把 LoongArch `ld.w` 写成
  零扩展的错误（实际为符号扩展，0/1 不受影响）。最终报告还漏掉 LA64 两套 `test_brk`
  各少 2 分；GPT/Codex 根据原始 judge JSON 恢复精确失败集合，没有照抄模型摘要。
- Verification: RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 21/21 PASS，新探针明确为第 20 项；
  `mask=0x003` 初赛分别为 312/314、308/314，四组 END、`online_mask=0xff`、源码前后指纹和
  精确接受失败集合均完成核对。测试未遗留用户工具桩、临时源码或调试字段。

### Case 28: SMP TCB `cpus_allowed` 与 placement 约束

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b31-cpus-allowed-summary.md`；原始审查 prompt、模型输出和
  四份 Docker/QEMU 日志只保存于本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux/DragonOS 选择数据模型，独立审计构造、继承、内存序、
  runqueue 锁序和原始 TAP/judge JSON；DeepSeek 负责冻结 diff 只读审查、串行执行四项
  Docker/QEMU 门禁和归纳结果，不修改源码、不提交、不 push。
- Problem: 调度器已能显式把任务发布/迁移到 AP，但 TCB 之前没有权威 CPU 允许集。
  错误目标只会在更后面以 owner 异常暴露，blocked wake 在 `last_cpu` 失效后也只能回退 CPU0。
- Human action: TCB 增加初始不变的 `cpus_allowed`；普通任务 CPU0-only，clone 继承，
  exec 保留，定向 ktest 任务收紧为单 CPU。Publish、yield requeue 和 blocked wake 在改变
  owner 前都 fail-stop 检查 mask；wake 在 allowed/online/scheduler/non-stopped 交集中优先
  `last_cpu`。没有新增调度状态或锁。
- AI adjudication: 采纳 DeepSeek 把 ktest 构造器收紧为 `pub(crate)` 的建议；纠正其
  “New 意味着创建者独占整个 TCB”的过强描述，准确契约是创建路径独占 mask 写入
  与首次发布时序。同时拒绝“测试没有任何盲区”的绝对表述，并从原始 JSON
  恢复 RV64 失 2 分、LA64 失 6 分的准确报告。
- Verification: 双架构 8 核 SMP focused 均为 21/21 PASS，第 11/12/20 项分别覆盖定向发布、
  blocked wake 和 yield 迁移；`mask=0x003` 初赛 RV64 312/314、LA64 308/314，四组 END、
  `online_mask=0xff`、精确失败集合和源码前后指纹均已核对。

### Case 29: SMP 用户返回 RESCHEDULE 安全点

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b33-user-return-reschedule-summary.md`；原始 prompt、
  DeepSeek 报告和四份 Docker/QEMU 日志只保存于本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计统一安全点、IRQ/内存序和反假通过用户 probe，独立核对 Linux
  上游、源码指纹、TAP 与 judge JSON；DeepSeek 负责冻结只读审查、通过 allowlist runner
  串行执行四项测试并汇总。提交仍由 GPT 在用户批准后执行。
- Problem: 远端 RESCHEDULE 只能唤醒 AP idle；运行中的用户任务即使在 syscall 窗口收到
  IPI，也只留下 `need_resched`，返回用户态前不会消费。直接实现动态 affinity 会使被 mask
  排除的 Running owner 缺少及时切出机制。
- Human action: 抽出唯一 `take_reschedule_request()`，由 AP idle 和用户返回共享；
  `run_task_safe_point()` 在 IRQ-off 窗口依次完成 deferred timer、Acquire 消费 IPI，再对
  两者最多调度一次。双架构 trap-return 在 `do_signal()` 前调用。focused probe 删除显式
  yield，由 CPU1 发送生产 IPI，并组合验证消费计数、getcpu 0→1、affinity、退出和回收。
- AI adjudication: 首次审查因 Codex 冻结期间继续编辑而 fail-closed，不作为模型证据；
  重试未发现 blocker。人工补上旧 pending 的基线清理，并纠正模型两处事实错误：
  `reschedule_count` 是 B33 新增字段；Linux 当前返回用户态循环先处理 need-resched、再处理
  signal work。最终结论以冻结源码的四份真实测试结果为准。
- Verification: RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 21/21 PASS，第 20 项明确为
  `smp::user_task_reschedules_from_ipi`；`mask=0x003` 初赛分别为 312/314、308/314，四组
  END、runner done、精确接受失败集合、无 mutation 和源码前后指纹均已独立核对。

### Case 30: SMP 当前线程运行期 affinity

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b34-self-affinity-summary.md`；DeepSeek prompt、child
  manifest 与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux/DragonOS 冻结 current-only 边界，设计 mask/target 内存序、
  实现 syscall 与双架构反假通过 probe，并独立裁决测试失败；DeepSeek 负责只读反例审查、
  受限 Docker 执行和结果汇总，不修改源码、不提交、不 push。
- Problem: 一次完成远程 Running、Queued 和 Blocked affinity 会立即要求新的 task/rq 串行化
  和队列间迁移协议，复杂度过高；旧 syscall 又只接受 bit0 并不真实修改 TCB。需要先交付一个
  不破坏六态所有权机的独立生产闭环。
- Human action: 仅允许 `pid=0` 或严格 current TID。写侧确认 `Running(source)` 与本地 current，
  先同步目标内核栈，再 Release 发布 mask/target，并立即在 syscall 安全点调度；源 idle 仍只锁
  一个目标 runqueue。目标放置比较 `nr_running + current`，远程 TID 返回 `EOPNOTSUPP`。
- AI adjudication: 首轮真实 RV64 timeout 暴露测试 runner 只开中断却不进入安全点；加入生产
  安全点后任务已完成迁移/退出，但旧等待器仍要求瞬态 zombie 队列保持非空。诊断模型把
  `zombies=0` 误判成计数器 bug；GPT/Codex 通过 `run_tasks()` 每轮 `take_zombie_tasks(64)` 的
  源码证据确认这是正常 drain，删除过时条件并移除临时快照。最终只接受冻结源码重跑结果。
- Verification: 最终任务 `smp-b34-self-affinity-validation-004` 串行完成四项；RV64/LA64 focused
  均为 21/21，第 20 项为 `smp::user_task_reschedules_and_sets_affinity`；初赛分别 312/314、
  308/314，精确失败集合、exit code、forbidden marker、timeout、mutation 与源码指纹均核对。

### Case 31: SMP 远程稳定 Blocked 线程 affinity

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b35-blocked-affinity-summary.md`；DeepSeek prompt、review、
  child manifest 与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux/DragonOS 的完整 task/rq 串行化，选择无 runnable owner 的
  Blocked 状态作为独立生产闭环，完成并发审计、实现、模型结论裁决与文档；DeepSeek 负责
  冻结只读反例审查、受限 Docker 串行执行和日志初步汇总，不修改源码、不提交、不 push。
- Problem: 只看 `TaskStatus::Blocked` 不能证明任务仍是正常睡眠 owner；exit/exec 会先从 registry
  摘除任务，稍后才标 Zombie。另一方面，直接给 Running/Queued 写 mask 会立即破坏 owner
  必须属于允许集的不变量，需要避免把三种状态揉进一次高风险修改。
- Human action: 写侧取得 `TASK_MANAGER` 后同时确认精确 Blocked 状态和 registry 中的同一 TCB
  指针，再 Release 写入 mask。wake 取得同一锁并 Acquire 读取，按新允许集发布到唯一 runqueue；
  两者谁先取得锁就决定成功更新或 `EOPNOTSUPP`。没有新增状态、锁、IPI reason 或队列搬运。
- AI adjudication: 与实现并行的首轮设计审查因 tracked diff 变化被包装器 fail-closed，只作为
  线索；冻结最终审查无 P0。GPT/Codex 拒绝把现有 `mark_zombie` 维护性建议混入本批，也纠正
  验证汇总中“LA64 test_brk 未执行”的错误：原始日志明确显示两套 basic 均运行且各得 1/3。
- Verification: `smp-b35-blocked-affinity-validation` 串行四项且源码指纹不变；RV64/LA64 focused
  均为 22/22，第 12/13/21/22 项分别覆盖旧 wake、新 mask 重定向、B34 用户 probe 和 STOP；
  初赛分别 312/314、308/314，精确失败集合未扩大。

### Case 32: SMP 远程稳定 Queued 线程 affinity

- Evidence: `docs/Work_Log/2026-07-30.md`、
  `docs/Work_Log/evidence/2026-07-30/smp-b36-queued-affinity-summary.md`；DeepSeek 三轮 review、
  child manifest 与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux/DragonOS 的 queued migration，设计唯一 owner 状态、实现、
  锁/内存序审计、模型结论裁决与文档；DeepSeek 负责设计反例、冻结源码复审、受限 Docker
  串行执行和日志汇总，不修改源码、不提交、不 push。
- Problem: 新 mask 排除 `Queued(source)` owner 时，既不能直接写 mask 留下非法 owner，也不能
  把目标 CPU 提前写成 owner；同时锁两把 rq 又违反现有锁序。nice hint 并发更新还可能让源
  队列派生计数按新 hint 错扣。
- Human action: 增加唯一必要的无 CPU `Migrating`，在 source 锁内摘除、无容器窗口发布 mask、
  target 锁内插入；所有 TLB 等待提前完成，锁外发送 RESCHEDULE。nice 更新读到旧 owner 时先
  重算旧队列再追踪新 owner；exit/exec 保留 `TASK_MANAGER -> 单个 RunQueue` 固定顺序。
- AI adjudication: 首次审查的 nice 竞态被采纳；收尾审查声称任务可在释放旧 rq 后双迁移回原
  CPU，但源码实际在解锁前读取状态，回迁还必须取得同一锁，故拒绝该反例。Codex 同时撤回
  自己的两阶段退出尝试，避免扩大 registry 外 Blocked 窗口。
- Verification: `smp-b36-queued-affinity-validation` 串行四项且源码指纹不变；RV64/LA64 focused
  均为 23/23，第 12/13/14/22/23 项覆盖旧 wake、Blocked/Queued affinity、用户 probe 和 STOP；
  初赛分别 312/314、308/314，精确失败集合未扩大。

### Case 33: SMP affinity-aware 新任务放置

- Evidence: `docs/Work_Log/2026-07-31.md`、
  `docs/Work_Log/evidence/2026-07-31/smp-b37-affinity-placement-summary.md`；DeepSeek 原始 prompt、
  child manifest 与 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 定位首次发布与继承 mask 冲突，设计无锁放置提示、
  实现、证据复核和文档；DeepSeek 做冻结只读反例审查、受限 Docker 串行验证和
  日志初步归纳，不修改源码、不提交、不 push。
- Problem: `fork`/`clone` 后的子线程继承父线程 mask，但旧 `publish_task()` 始终向
  CPU0 发布；当 mask 已排除 CPU0 时会直接触发 affinity owner 断言。另一方面，
  在 `TASK_MANAGER` 内取 processor 锁会扩大锁序和死锁风险。
- Human action: 用 `cpus_allowed & online & scheduler & !stopped` 确立候选集，再用
  `nr_running + current_present` 作放置质量提示；合法的偏好 CPU 在最小负载 `+1`
  内时保留局部性。提示不承担 owner 正确性，最终状态仍由单个 runqueue 锁内提交。
  scheduler-ready 前的 BSP init/ktest runner 是明确 CPU0 启动例外。
- AI adjudication: 首轮审查提示了启动 mask 空集与锁序风险，均被纳入；
  两个 wrapper 因工作树指纹在任务期间变化或旧容器挂载而 fail-closed，不当作代码失败；
  最终只采信指向正确 worktree 且源码指纹不变的冻结任务。
- Verification: `smp-b37-placement-validation-002` 串行通过 RV64/LA64 kernel build 与
  8 核 focused SMP 23/23；`smp-b37-preliminary-validation` 通过双架构初赛回归，
  RV64 312/314、LA64 308/314，失败仅为既有 busybox kill10 和 LA64 test_brk。

### Case 34: SMP 远程 Running/Blocking affinity

- Evidence: `docs/Work_Log/2026-07-31.md`、
  `docs/Work_Log/evidence/2026-07-31/smp-b38-running-affinity-summary.md`；DeepSeek prompt、child
  manifest 与 Docker/QEMU 原始日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责状态机边界、请求生命周期、锁序、实现、测试设计和最终事实裁决；
  DeepSeek 负责冻结 diff 只读复核、Docker 串行构建/测试及日志初步归纳，不修改源码、不提交、
  不 push。
- Problem: 远程写侧若直接把 Running task 的 mask 改成排除 owner，会立即破坏
  `Running(cpu) => mask contains cpu`；若仅发 IPI 就返回，又无法保证 syscall 返回时迁移已经
  生效。`Blocking(cpu)` 还是 owner 正从 current 切出的短暂窗口，不能被当作稳定 Blocked。
- Human action: 保留原调度状态机，为每个 TCB 增加至多一个远程 affinity 请求。请求者在锁内
  复核 owner 并安装 mask/target，锁外发 RESCHEDULE 后协作式等待；owner 在既有
  `finish_switch_out()` 安全点持请求槽、只锁目标 runqueue，提交 owner 交接后用原子 CAS 完成。
  阻塞或退出路径把尚未消费的请求标为 Retry，调用方再按稳定状态选择 Running、Queued 或
  Blocked 协议。真实嵌套关系是 `task.inner/TASK_MANAGER -> request slot -> 单个 RunQueue`，
  不存在反向 runqueue→request 路径。
- AI adjudication: 首轮验证发现 `manager.rs` 经模块重导出引用私有类型导致双架构编译失败；
  GPT/Codex 改为 sibling module 直接导入，避免扩大公开 API。最终报告又错误声称 owner 在取得
  runqueue 前释放请求锁，并低报 TAP 数；人工以源码与原始日志纠正，不采纳该描述。一次误用
  `cargo fmt -- <files>` 触发全 crate 格式化后已完整撤回，最终 diff 不含无关机械改写。
- Verification: 冻结源码 diff SHA-256 为
  `4875482b6e06f089eb1c3060a6c20259c902a66a6e44c19ca746c6b42c44b465`。RV64/LA64
  kernel build 均 exit 0；双架构 `CORE_NUM=8 KTEST=smp` 均为 24/24，新第 15 项和终态 STOP
  均通过。初赛为 RV64 312/314、LA64 308/314，精确失败集合未扩大。动态证据覆盖单请求者
  Running owner；并发多写者及确定性的 Blocking 交界仍标为未动态运行。

### Case 35: SMP Per-CPU 调度 tick 与 CPU0 全局 timer owner

- Evidence: `docs/Work_Log/2026-07-31.md`、
  `docs/Work_Log/evidence/2026-07-31/smp-b39-percpu-timer-summary.md`；DeepSeek prompt、child
  manifest 与 Docker/QEMU 原始日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 负责对照双架构官方 timer 规范，划分本地 quantum 与全局 callback
  所有权、实现、测试证明设计和最终事实裁决；DeepSeek 负责设计反例提示、冻结源码下的
  Docker 串行构建/测试与日志归纳，不修改源码、不提交、不 push。
- Problem: 旧实现只有 CPU0 开 timer，且所有 CPU 共用一个调度 deadline。普通任务若放宽
  到 AP，一个不执行 syscall/yield 的 CPU-bound 用户任务就无法被切出；直接让每个 AP 执行
  旧 timer deferred work，又会重复消费 timeout、timerfd 和网络 poll 等全局状态。
- Human action: 每个 `PerCpu` 保存独立 100 Hz 绝对 deadline；所有 CPU 的 hard IRQ 都只发布
  deferred 标志，安全点只推进本 CPU quantum。CPU0 额外串行消费全局 timer queue、timeout、
  timerfd 和 net poll，并按本地 tick 与全局最早 deadline 的较小值编程 one-shot timer。
  AP 插入更早全局 timer 时先释放 queue 锁，再通过独立 `TIMER_REPROGRAM` reason 请求 CPU0。
  两架构均采用“先发布/编程 deadline，再开放中断源”，LoongArch 开 timer 时保留 ECFG 中
  已经开放的 IPI bit；LA64 stable-counter 频率改为 CPU0 Release 发布、各 CPU Acquire
  读取的原子值，删除本路径最后一个依赖单核读写时序的 `static mut`。
- AI adjudication: 设计审查任务因 Codex 同期继续编辑 tracked diff 被包装器 fail-closed，
  其 stdout 仅作为反例线索；DeepSeek 提醒同时检查 timer 与 reprogram 两种 pending、隔离
  AP 的全局 callback、覆盖 AP idle 安全点，三项均被纳入。它建议复用 `RESCHEDULE` bit，
  GPT/Codex 拒绝：纯 deadline 重编程不应伪造任务切换请求。首轮六项验证均在编译期暴露
  `hal::arch` 中间层漏重导出；补齐双架构两个 re-export 后重新冻结并完整验证，失败批次
  不计作通过证据。原子频率复检后 DeepSeek 建议直接提交；人工继续沿 AP deferred 调用链
  发现性能快照会读取 FS/net 并打印 console，因此拒绝提前收口，把格式化快照限制到 CPU0。
- Verification: 生产源码 diff SHA-256 为
  `3d3670bfc12e1702d0256dd9d12c23666a9a74ea40fc257901e36b10af2431e6`。RV64/LA64
  kernel build 均 exit 0；双架构 `CORE_NUM=8 KTEST=smp` 均为 25/25，新增第 8 项使用
  CPU1 无 syscall/yield 用户死循环证明真实本地 timer 能切出任务，online mask 均为
  `0xff`。初赛为 RV64 312/314、LA64 308/314，精确失败集合未扩大。人工收口原子频率和
  CPU0-only 快照后，最终冻结又通过双架构 build 与 LA64 25/25；该三项完整 tracked diff
  指纹均为 `eec8bfde6f0b626296b7002bb83eb6b079b7f12e597e77d248c16d7e43bafbd6`，
  before/after 一致。

### Case 36: SMP 线程组退出与多线程 exec

- Evidence: `docs/Work_Log/2026-07-31.md`、
  `docs/Work_Log/evidence/2026-07-31/smp-b40-group-exit-summary.md` 和
  `docs/Work_Log/evidence/2026-07-31/smp-b41-exec-summary.md`；DeepSeek prompt、manifest
  与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux `do_group_exit()/de_thread()` 与 DragonOS 当前线程退出
  原则，设计线程组门禁、live-token 线性化、等待点退栈、实现与最终事实裁决；DeepSeek
  负责冻结 diff 的反例审查、受限 Docker 串行执行和原始日志归纳，不修改源码、不提交、不 push。
- Problem: 旧 exit/exec 会由发起 CPU 从远端 runqueue 摘除 sibling 并直接释放其用户映射
  和 TCB；目标 CPU 可能仍运行在这些资源上。多线程 exec 还不能在 sibling 离开旧 MM 前
  替换地址空间，late clone 也不能漏出停止快照。
- Human action: B40 让永久 group exit 在线程组锁内固定退出码、关闭首次发布并发送
  SIGKILL/wake/RESCHEDULE，所有 sibling 在 owner CPU 安全点自清理。B41 在同一锁域增加
  可恢复 `ExecSession + Completion`，拒绝 concurrent exec/late thread publish；live count
  只在用户资源撤销和 TLB ack 后递减到 1，owner 才安装新映像并重新开门。永久 group exit
  优先覆盖 exec，WaitQueue/Completion/vfork 会因生命周期停止请求安全退栈。
- AI adjudication: B40 初轮冻结暴露 `then_some` 参数提前求值造成零值下溢，人工终止旧任务
  后修正；B41 验证包装器因模型误重复提交 RV64 preliminary 而 fail-closed，前五个冻结
  child 仍逐项有效，GPT/Codex 另起只允许一次 LA64 preliminary 的补测闭合门禁。模型报告
  中关于是否存在 `exec.wait()` 和是否可跳过 LA 回归的错误均未采纳。
- Verification: B40 已提交为 `f1797a85`。B41 冻结生产 diff SHA-256 为
  `dff2949af9e355cc1c5382f869e26068d014307e28de1872dc2507a457949d55`；双架构
  kernel build exit 0，RV64/LA64 `CORE_NUM=8 KTEST=smp` 均为 27/27，新增
  `exec_stops_remote_sibling` 与终态 STOP 明确通过。初赛 `mask=0x003` 为 RV64
  312/314、LA64 308/314，精确失败集合与既有基线一致，clone/fork/exec/exit/wait
  项满分；所有有效 child 均无源码 mutation。

### Case 37: SMP trap context 与 signal 用户访存锁边界

- Evidence: `docs/Work_Log/2026-07-31.md` 及同日 B45—B48 evidence；DeepSeek prompt、
  manifest 与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux v6.6 通用及 RV64/LA64 signal 路径，设计 current-owner
  借用边界、实现和最终事实裁决；DeepSeek 负责冻结只读设计审查、受限 Docker 串行验证
  和日志初步归纳，不修改源码、不提交、不 push。
- Problem: 旧 current helper 可从临时 `task.inner` guard 返回伪造的全局可变 trap 引用；
  sigreturn 和 handler frame 投递又跨 faultable 用户访存持有普通任务锁。
- Human action: B45 将 trap 可变借用绑定到 inner guard；B46 以“锁内快照、锁外读取、
  锁内提交”恢复 frame；B47 对称地在锁外写完整 `SigInfo + UserContext`，成功后才发布
  handler PC/mask；B48 又把 `sigaction/sigprocmask/sigaltstack` 的输入读取和旧值写回
  移到普通锁外，并把 sigmask 用户 ABI 固定为低 64 位。四步都复用 current owner
  不变量，没有新增 generation、事务对象或状态机。
- AI adjudication: DeepSeek 的 B47 结论遗漏 LA64 两项 `test_brk` partial failure，并
  误把普通 PID1 SIGCHLD 路径描述成动态覆盖所有 signal flag；B48 又误判 Linux 会在
  `rt_sigprocmask`/`sigaltstack` 内部操作失败后继续 copyout 旧值。GPT/Codex 分别根据
  原始 judge JSON、action 源码和 Linux v6.6 syscall wrapper 修正，没有采纳错误结论。
- Verification: B45/B46 已提交为 `12b54ce0`/`95538a23`；B47 冻结生产源码 diff
  SHA-256 为 `0cda317e4a5f7ed640136135e57634c9cc16555a0d5aa3fc3da86e6ed5b255bb`。
  B47 双架构 `CORE_NUM=8 mask=0x003` 为 RV64 312/314、LA64 308/314，精确失败集合与
  B46 一致。B48 冻结生产源码 diff SHA-256 为
  `33a8f1ccbf41278a8132b928542d928ca0c485ce3ee7f6fa6bd079ee971f7644`，同一门禁仍为
  RV64 312/314、LA64 308/314；四个 B47/B48 child 均无源码 mutation。

### Case 38: SMP 空闲核 work stealing

- Evidence: `docs/Work_Log/2026-07-31.md`、
  `docs/Work_Log/evidence/2026-07-31/smp-b49-work-steal-summary.md`；DeepSeek prompt、manifest
  与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计 victim 选择、单 owner 交接、显式迁移排除和确定性测试，负责
  实现与最终裁决；DeepSeek 只读审查并通过受限网关串行运行双架构 Docker/QEMU，不修改
  源码、不提交、不 push。
- Problem: 本地 runqueue 为空的 CPU 不能分担其它 CPU 的 backlog；直接跨队列搬运又容易
  同时持双锁、在锁内等待 kernel-TLB 同步，或与 queued affinity 写侧争夺同一 TCB。
- Human action: steal 先在 victim 锁内克隆候选，锁外同步本地 kernel mapping，重锁复核
  成员/状态/mask/migration target，再复用 `Queued -> Migrating -> Running` 交接。focused
  测试先固定 CPU0 队列，再扩 affinity，消除发布后检查与 AP timer 的竞态。
- AI adjudication: 首轮冻结期间补安全条件触发父任务 fail-closed，其模型 ACCEPT 未采信；
  最终冻结审查确认锁和状态不变量。模型建议立即开放默认全核 affinity 被否决，原因是共享
  FS/net/driver 尚未通过 Phase 5 审计。
- Verification: 最终 tracked diff SHA-256 为
  `6e9895ec1f28e873f67b2d2425e1ca550930db52f321b12dc2e8ef5c01a9f390`；RV64/LA64
  `CORE_NUM=8 KTEST=smp` 均为 31/31。生产逻辑冻结后的初赛仍为 RV64 312/314、LA64
  308/314，失败身份不变；有效 child 均无源码 mutation、panic 或 timeout。

### Case 39: SMP Per-CPU zombie 回收

- Evidence: `docs/Work_Log/2026-08-01.md`、
  `docs/Work_Log/evidence/2026-08-01/smp-b50-local-zombie-summary.md`；DeepSeek prompt、manifest
  与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计 Per-CPU Arc owner、跨 CPU reap 和可区分旧实现的
  focused 用例，完成实现与最终裁决；DeepSeek 只读审查并通过受限网关
  串行执行双架构 focused/初赛回归，不修改源码、不提交、不 push。
- Problem: AP 退出后的 TCB 仍进入全局 `TASK_MANAGER.zombie_queue`，最后由
  CPU0 代为析构；这与 Per-CPU current/runqueue 所有权不对称，并让 AP 退出
  竞争全局锁。
- Human action: `finish_switch_out()` 在退出 CPU 的 idle 栈把最后调度 Arc 交给
  `CpuTaskState.local_zombies`，同 CPU 在下一次 dispatch 前 drop；按 pid 回收逐队
  扫描，不嵌套 `TASK_MANAGER`/本地队列锁，不在容器锁内扩容承接 Vec
  或执行 TCB 析构链。
- AI adjudication: DeepSeek 给出 `ACCEPT`，但误称 AP 直接执行 kernel-stack 退休
  shootdown，且漏报 LA64 两个 `test_brk` partial failure；GPT/Codex 以调度源码
  和 child 原始 judge JSON 纠正。固定容量 zombie 容器建议也未采纳，因其
  未定义安全的溢出协议。
- Verification: 冻结 tracked diff SHA-256 为
  `9c7d88145430f9e6435f32b1e2ef428fa1b70aa6b10f23007c496dc6314d0a03`；RV64/LA64
  `CORE_NUM=8 KTEST=smp` 均为 32/32。初赛仍为 RV64 312/314、LA64 308/314，
  精确失败集合不变；四个 child 均无源码 mutation、panic 或 timeout。

### Case 40: SMP 精确 active MM 驻留与安全切离

- Evidence: `docs/Work_Log/2026-08-01.md`、
  `docs/Work_Log/evidence/2026-08-01/smp-b51-active-mm-summary.md`；DeepSeek prompt、manifest
  与原始 Docker/QEMU 日志只保存在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 Linux 的 `switch_mm/leave_mm` 与 membarrier 调度屏障语义，
  设计 writer/enter/leave 的共同 VM 锁线性化点、实现并裁决测试结果；DeepSeek 只读审查
  锁序，并通过受限网关串行执行双架构构建、focused 与初赛测试，不修改、提交或上传源码。
- Problem: B22 的 `cached_cpus` 只增不减，已经切离 MM 的 CPU 会永久成为 shootdown 和
  PRIVATE_EXPEDITED 目标；直接清 bit 又会在 PTE writer、调度切离和重新进入之间遗漏失效，
  尤其不能把 `targets=0` 错当成硬件中不存在旧 ASID 翻译。
- Implemented change: 每 CPU 保存当前用户 MM 的精确 Arc；idle 栈在改变 current owner 前
  执行 leave full fence 并清 active bit，trap-return 通过 `switch_user_vm()` 进入新 MM。
  PTE 修改即使没有 active target 也推进 generation，使旧翻译只能在下次进入前补刷后使用。
  exec 后依靠 Per-CPU Arc 清理旧 MM，而不是重新读取已替换的 `process.vm`。
- AI adjudication: 首轮 RV64 membarrier RED 是 helper 主动安全点切离后仍要求历史 IPI 的
  测试假设；随后双架构第二轮 group-exit RED 是测试先观察 TCB/live-token、未等待 PCB
  `finish_exit()`。GPT/Codex 分别固定测试分支和最终完成条件，没有修改生产协议或放宽超时。
  DeepSeek 还把 active=0 误述为“没有旧 TLB”，并把每架构已经包含两轮的 65 个 TAP 点再次
  乘以 repeat 报成 260；最终证据按原始日志纠正为双架构合计 130 个检查点。
- Verification: 最终冻结 tracked diff SHA-256 为
  `c0e54db406bce69031947d152b5502b615f26f747cb647a690256e9ffd5be1e8`；RV64/LA64
  `CORE_NUM=8 KTEST=smp KREPEAT=2` 均为 65/65。初赛保持 RV64 312/314、LA64
  308/314，精确失败集合与 B50 一致；四个最终 child 均无源码 mutation、panic 或 timeout。

### Case 41: SMP 有界连续用户 TLB shootdown

- Evidence: `docs/Work_Log/2026-08-01.md`、
  `docs/Work_Log/evidence/2026-08-01/smp-b52-range-shootdown-summary.md`；只读审查 prompt、
  manifest 和原始 Docker/QEMU 日志仅保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 对照 SBI RFENCE、LoongArch INVTLB 和 Linux RISC-V range 策略，
  设计 64 页 IRQ 上限、实现双架构后端并人工裁决原始日志；DeepSeek 只读审查范围合并、
  固定槽内存序和 frame 生命周期，再通过受限网关串行执行六项 Docker 门禁，不修改或提交源码。
- Problem: B24/B26 只能精准处理单个 VPN；同一 `munmap/mprotect/CoW` 写操作一旦修改第二页，
  `MmuGather` 就升级为整个用户地址空间全刷。直接发布动态 VPN 列表又会让 hard-IRQ 分配、
  无界遍历或引入另一套复杂 batch。
- Implemented change: `FlushRange::Range` 保存最小包围半开区间，最多 64 页；稀疏空洞允许
  多刷，超过上限和页表层级变化进入 `Full`。RV64 把物理 hart mask、字节 start/size 和
  ASID 交给 SBI，LA64 固定槽发布 ASID/start/count 并按偶/奇硬件页对步进。主链仍是
  `record_change -> seal -> execute`，同步完成后才释放退休 frame。
- AI adjudication: 接受只读审查对溢出、Release/Acquire、奇数边界和 timeout 不复用槽的
  PASS 结论；模型建议追加 KREPEAT=10 未采纳，因为当前差异已由并发 8 槽、3 页区间、
  65 页全刷和初赛门禁覆盖。模型摘要未给出 LA64 basic 精确计数，GPT/Codex 从完整 judge
  表核对为 308/314，并保留两套 `test_brk` 与两套 `busybox kill 10` 的既有失败身份。
- Verification: 冻结 tracked diff SHA-256 为
  `1d8909e1a37843be5673affe6fe6b0952076a54f0acb9ba9f7a2a687047a2c43`；双架构
  `CORE_NUM=8 KTEST=smp` 均为 33/33。初赛仍为 RV64 312/314、LA64 308/314；六个
  child 均 exit 0、`online_mask=0xff`（QEMU 项）、无源码 mutation、panic 或 timeout。

### Case 42: SMP 真实用户访存 stale-TLB 证明

- Evidence: `docs/Work_Log/2026-08-01.md`、
  `docs/Work_Log/evidence/2026-08-01/smp-b53-stale-tlb-user-access-summary.md`；DeepSeek
  prompt、manifest 和原始 Docker/QEMU 日志只保留在本地忽略的 `cc-codex/`。
- AI roles: GPT/Codex 设计真实 CoW 用户 victim、fixed-slot MM 生命周期、timer 隔离和失败
  清理并裁决原始日志；DeepSeek 先只读审查设计，再通过受限 Docker 网关执行冻结验证和首错
  分析，不修改或提交源码。
- Problem: B52 的 request/ack、range payload 和 frame-retirement 用例仍可能在精准 handler
  损坏时被下一次 trap-return generation catch-up 全刷掩盖；计数器增长不能证明 CPU 真正停止
  使用旧 PPN。
- Implemented change: CPU1 用户汇编先持续读取保留的旧 frame，CPU0 经正式 private CoW
  更新 PTE，等待区间 shootdown 后才向新 frame 写 canary。软件 handler 在失效后、ack 前
  发布目标 CPU observed generation；测试静默 timer，并用 FIFO restore helper 拒绝意外调度。
  RV64 RFENCE 和 full-flush 不携带 fixed-slot MM 指针，仍在同步返回后由发送方统一记账。
- AI adjudication: 拒绝为测试新增 `replace_user_frame_with()` 生产 API，复用现有 CoW 主链；
  也纠正了模型把 `cpu_tlb_is_current()==false` 当作无 trap 证据的错误。首轮 focused 暴露
  `FlushRange::Full` 误带精准 generation 的断言，保留断言并在参数构造分支修根因；模型把
  已通过的 #25 误报为失败，最终按原始 TAP 顺序记为下一 full-retirement 用例启动前 panic。
- Verification: 最终可执行 tracked diff SHA-256 为
  `bb213434751e37a470d1dffe70c776c9d66aec5bdec456a237db2e71335aa396`；双架构 normal
  build exit 0，`CORE_NUM=8 KTEST=smp KREPEAT=2` 均 67/67、`online_mask=0xff`，无
  mutation、panic、timeout 或 fatal。首轮同功能快照的初赛保持 RV64 312/314、LA64
  308/314；最终一处分支修正按风险只复跑 build/focused，证据边界已明确记录。

## 6. 质量控制与验证方式

AI 输出进入项目之前，采用以下质量控制流程：

1. **Human review**：维护者阅读 AI 建议和 diff，确认语义、边界条件、错误码、锁顺序和架构一致性。
2. **Dual-architecture build**：内核修改按项目规则分别执行 rv64 和 la64 build，例如 `make rv64-kernel-build-only` 与 `make la64-kernel-build-only`。
3. **QEMU integration tests**：关键功能通过 rv64 / la64 QEMU 启动与相关测试组验证，包括 basic、busybox、LTP focused、lmbench、iperf、netperf 等。
4. **Focused regression tests**：针对具体 bug 使用 focused LTP include、mask、inline runner 或 custom smoke test 验证。
5. **Performance before/after comparison**：性能优化使用同一镜像、同一测试项进行前后对比，避免只依据 AI 判断。
6. **Documentation fact-check**：生成文档需对照源码、commit log 和测试记录；Oracle review 指出的问题由人工修正。
7. **Work log recording**：重要变更写入 `docs/Work_Log.md`，包含文件、验证结果和备注。
8. **No direct trust in AI output**：AI 结论不作为最终证明；最终依据是源码、构建结果、QEMU 日志、测试输出和人工审查。

## 7. Commit 证据表

以下表格列出关键 AI 使用 commit，非完整清单。完整记录可通过 `git log --grep='Copilot'`、`git log --grep='Sisyphus'`、`git log --grep='Oracle'` 和 `docs/Work_Log.md` 追溯。

| Date | Commit | Area | AI evidence | Outcome |
|---:|---|---|---|---|
| 2026-04-24 | `c7f99d8e` | Network / netperf | `Co-authored-by: Copilot <copilot@github.com>` | 跑通 netperf |
| 2026-04-25 | `89272026` | Network blocking model | `Co-authored-by: Copilot <copilot@github.com>` | 将 loop 上移到 syscall 层，采用 wait_io |
| 2026-05-04 | `4ee10370` | Socket abstraction | `Co-authored-by: Copilot <copilot@github.com>` | 增加 socket abstraction |
| 2026-05-05 | `824c654d` | UNIX socket | `Co-authored-by: Copilot <copilot@github.com>` | 初步实现 UNIX socket |
| 2026-05-06 | `50d97f0b` | Routing device | `Co-authored-by: Copilot <copilot@github.com>` | routing device 与宿主机交互 |
| 2026-05-19 | `2a6cb25c` | LTP zero score / FS / MM | `Root cause analysis by Oracle identified three bugs` | 修复 `/dev/null ENOSYS`、missing symlinks、MAP_SHARED SIGBUS |
| 2026-06-16 | `07dda312` | FS performance | `Ultraworked with Sisyphus` | 5-target FS optimization |
| 2026-06-16 | `88996548` | FS correctness/performance | `Oracle review identified root causes`; `Co-authored-by: Sisyphus` | 修复 sync/datasync、dirty inode cache、dentry cache regression |
| 2026-06-19 | `4a907eb1` | perf_diag | `Co-authored-by: Sisyphus` | 添加 P0 diagnostic counters |
| 2026-06-19 | `3a4bc048` | Drift analysis | `detects anomalies using Oracle decision tree`; `Co-authored-by: Sisyphus` | 新增 `scripts/analyze_drift.py` |
| 2026-06-20 | `c9399565` | Buddy allocator | `Oracle-identified issues` | 修复 bitmap guard ordering 和 fallback |
| 2026-06-28 | `364bb5d6` | PageCache read-ahead | `Root cause identified by Oracle analysis` | 修复非连续 batch read 导致 la64 指令损坏 |
| 2026-06-29 | `fd735048` | Judge docs | `Ultraworked with Sisyphus`; `Co-authored-by: Sisyphus` | 新增 Technical Report 和 Engineering Casebook |
| 2026-06-29 | `81a24d2a` | Documentation fact-check | `Oracle-reviewed fixes`; `Co-authored-by: Sisyphus` | 修复多处文档事实问题 |
| 2026-06-29 | `9b054de8` | Final judge doc review | `final Oracle review fixes`; `Co-authored-by: Sisyphus` | 终审修复评审文档 |

## 8. Work_Log 证据表

| Work log reference | Topic | AI usage evidence |
|---|---|---|
| `docs/Work_Log.md:165-265` | Judge-facing docs 多轮修复 | Oracle review 发现并修复文档事实不准确、虚构抽象和绝对化表述 |
| `docs/Work_Log.md:454-456` | PageCache read-ahead bug | 记录 batch 连续性假设破裂导致 la64 executable page corruption 的根因 |
| `docs/Work_Log.md:668-692` | FS performance plan | Oracle 给出 FS 性能优化优先级矩阵 |
| `docs/Work_Log.md:717-777` | Buddy allocator scan drift | 记录 drift 调试、bitmap guard 方案与验证 |
| `docs/Work_Log.md:824-840` | `drift_window` and `analyze_drift.py` | 记录 Oracle decision tree 和自动漂移分析脚本 |
| `docs/Work_Log.md:1093-1125` | Network optimization | 记录 iperf TCP 34x、netperf CRR +19% 的多轮优化 |
| `docs/Work_Log.md:1455-1658` | Timer subsystem | 记录 timer deadline / one-shot / timekeeping 修复与测试 |
| `docs/Work_Log.md:5963-6006` | LTP zero score | 记录 Oracle 分析后发现 `/dev/null ENOSYS`、missing symlinks、MAP_SHARED SIGBUS 等问题 |
| `docs/Work_Log/2026-07-17.md` | lwext4 inode-incarnation cache isolation | 记录 Oracle 根因审查、直接 counter log 与 RV64 4/4 focused QEMU 验证 |
| `docs/Work_Log/2026-07-21.md`、`docs/Work_Log/evidence/2026-07-21/la64-mmap-arena-red-20260721T053537+0800/`、`docs/Work_Log/evidence/2026-07-21/la64-mmap-boundary-final-20260721T060040+0800/`、`docs/Work_Log/evidence/2026-07-21/la64-mmap-boundary-artifact-binding-supplement-20260721T063550+0800/` | LA64 mmap arena 边界与 trap-context 窗口 | 记录旧范围导致的非固定 mmap RED、最终 `[USR_MMAP_BASE, TRAP_CONTEXT_BASE)` 修正、固定映射拒绝规则、RV64/LA64 TAP 1..6、LA64 `STATE=PASS STATUS=0`、真实 `/regression` ELF 绑定及 Oracle 最终验收 |
| `docs/Work_Log/2026-07-22.md` | Canonical normal run facade | 记录 Oracle 发现 root logo/preflight 重复调用、target-scoped `.NOTPARALLEL` 修复、dry-run once-only 与 `-j8` invalid-input contracts |
| `docs/Work_Log/2026-07-25.md`、`docs/Work_Log/evidence/2026-07-25/smp-b08-*` | 双架构 SMP AP idle stack | 记录 DeepSeek 只读审查、人工裁决、RV64/LA64 8 核 3/3 PASS 和 ELF 反汇编证据 |
| `docs/Work_Log/2026-07-27.md`、`docs/Work_Log/evidence/2026-07-27/smp-b15-summary.md` | SMP 调度所有权与阻塞唤醒交接 | 记录 DeepSeek 冻结源码审查、人工收敛六态状态机、双架构 4 核 SMP 19/19 PASS 与证据边界 |
| `docs/Work_Log/2026-07-27.md`、`docs/Work_Log/evidence/2026-07-27/smp-b16-summary.md` | SMP 本地 TLB batch | 记录 DeepSeek 生命周期/冻结 diff 只读审查、GPT/Codex 裁决、双架构 MM ktest 8/8 PASS 与远端 shootdown NOT RUN 边界 |
| `docs/Work_Log/2026-07-27.md` | SMP Per-CPU current 槽 | 记录 DeepSeek 首轮 RED/最终只读验证、GPT/Codex Arc 生命周期裁决、双架构 4 核 SMP 19/19 PASS 与 B18 边界 |
| `docs/Work_Log/2026-07-28.md` | SMP 初赛非回归门禁 | 记录 DeepSeek 双架构 8 核执行、RV64 新增失分、单核判别、人工日志复核与递增基线规则 |
| `docs/Work_Log/2026-07-28.md` | RV64 trap-return 半恢复现场竞态 | 记录提交撤回、DeepSeek 复现实验的采纳边界、ELF/CSR 指令级根因、双架构修复验证和本地 Worker 领取竞态修复 |
| `docs/Work_Log/2026-07-28.md`、`docs/Work_Log/evidence/2026-07-28/smp-b18-runqueue-summary.md` | SMP Per-CPU RunQueue | 记录 DeepSeek 冻结审查与双架构 8 核 Docker 门禁、GPT/Codex 锁序裁决、19/19 PASS 和 AP 调度 NOT RUN 边界 |
| `docs/Work_Log/2026-07-28.md`、`docs/Work_Log/evidence/2026-07-28/smp-b19-ap-scheduler-summary.md` | SMP AP 本地调度闭环 | 记录 DeepSeek 对首次 dispatch 卡死的页表根定因、GPT/Codex 映射发布协议裁决、双架构 8 核 23/23 PASS 与用户任务仍固定 CPU0 的边界 |
| `docs/Work_Log/2026-07-28.md`、`docs/Work_Log/evidence/2026-07-28/smp-b20-remote-wake-summary.md` | SMP 远程 blocked wake | 记录 `last_cpu`、批量 wake 锁外 IPI、DeepSeek 机械验证与人工裁决、双架构 8 核 25/25 PASS 及用户迁移 NOT RUN 边界 |
| `docs/Work_Log/2026-07-28.md`、`docs/Work_Log/evidence/2026-07-28/smp-b21-kernel-mapping-retirement-summary.md` | SMP kernel-global 撤映射与栈回收 | 记录全核 TLB sequence/ack、固定退休队列、安全点回收、DeepSeek 建议采纳/拒绝边界、双架构 27/27 与初赛非回归结果 |
| `docs/Work_Log/2026-07-28.md`、`docs/Work_Log/evidence/2026-07-28/smp-b22-user-tlb-foundation-summary.md` | SMP 用户 MM 激活与 user-TLB IPI 基础设施 | 记录 VM 锁死锁边界、单调 cached mask/generation、独立 sequence、DeepSeek 跨原子建议裁决、双架构 29/29 与完整 shootdown NOT RUN 边界 |
| `docs/Work_Log/2026-07-29.md`、`docs/Work_Log/evidence/2026-07-29/smp-b23-user-tlb-flush-summary.md` | SMP 用户 PTE shootdown 与 MMU 接口收敛 | 记录锁内收集/锁外同步、frame 退休、DeepSeek 只读复核、双架构 focused 与初赛非回归证据 |
| `docs/Work_Log/2026-07-29.md`、`docs/Work_Log/evidence/2026-07-29/smp-b24-rfence-summary.md` | SMP RV64 页级 RFENCE 与 IPI fallback | 记录 SBI/Linux/DragonOS 对照、逻辑/物理 hart mask、DeepSeek 门禁与人工证据边界裁决 |
| `docs/Work_Log/2026-07-29.md`、`docs/Work_Log/evidence/2026-07-29/smp-b26-precise-shootdown-summary.md` | SMP LoongArch ASID+VPN 精准 shootdown | 记录固定 per-CPU slot、页对硬件粒度、DeepSeek 冻结验证与人工证据边界修正 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b27-rv64-asid-summary.md` | SMP RV64 MM-owned ASID 与页级 RFENCE FID 2 | 记录 ASIDLEN 探测、flush-before-reuse、trap-return IRQ-off 交接、DeepSeek 只读审查与双架构 Docker/QEMU 门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b28-ap-user-summary.md` | SMP 受控 AP 用户态闭环 | 记录远程首次发布顺序、trap owner/noreturn Arc、RW→RX 探针、双架构 21/21 与初赛非回归边界 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b29-yield-migration-summary.md` | SMP 用户任务显式 yield 迁移 | 记录一次性目标、单 runqueue owner 交接、首轮 shootdown ack RED、DeepSeek 误判裁决、双架构最终 21/21 与初赛非回归 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b30-getcpu-summary.md` | SMP 真实逻辑 CPU 查询 | 记录 getcpu ABI、迁移前后 0/1 反假通过、DeepSeek 结论纠错与双架构门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b31-cpus-allowed-summary.md` | SMP TCB affinity 调度约束 | 记录 `cpus_allowed` 构造/继承、三条 placement 硬约束、既有锁序、DeepSeek 只读审查与双架构 8 核门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b32-getaffinity-summary.md` | SMP 线程 affinity 只读 ABI | 记录 Linux raw 返回值/TID 语义、锁外 uaccess、双架构 probe 与冻结初赛门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b33-user-return-reschedule-summary.md` | SMP 用户返回 RESCHEDULE 安全点 | 记录 IRQ-off 合并入口、Release/Acquire、IPI 驱动迁移、模型结论纠错与双架构 8 核门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b34-self-affinity-summary.md` | SMP 当前线程运行期 affinity | 记录 current-only 阶段边界、mask/target 发布、测试等待器首错、DeepSeek 误判裁决与双架构冻结门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b35-blocked-affinity-summary.md` | SMP 远程 Blocked 线程 affinity | 记录状态/registry 双重确认、与 wake 共锁线性化、DeepSeek 冻结审查裁决及双架构 8 核门禁 |
| `docs/Work_Log/2026-07-30.md`、`docs/Work_Log/evidence/2026-07-30/smp-b36-queued-affinity-summary.md` | SMP 远程 Queued 线程 affinity | 记录 `Migrating` 唯一 owner、单 rq 搬队、nice/exit 竞态裁决及双架构 8 核门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b37-affinity-placement-summary.md` | SMP affinity-aware 新任务放置 | 记录继承 mask 与固定 CPU0 发布冲突、无锁负载提示边界、DeepSeek 冻结审查裁决及双架构 8 核门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b38-running-affinity-summary.md` | SMP 远程 Running/Blocking affinity | 记录单槽请求、owner 安全点完成、真实锁序、DeepSeek 结论纠错及双架构 8 核门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b39-percpu-timer-summary.md` | SMP Per-CPU 调度 tick | 记录本地 quantum/CPU0 全局 callback 边界、官方规范对照、DeepSeek RED/冻结验证裁决及双架构 8 核门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b40-group-exit-summary.md` | SMP 跨 CPU 线程组退出 | 记录永久 gate、owner 自清理、live-token ack、DeepSeek 反例审查及双架构 26/26/初赛门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b41-exec-summary.md` | SMP 多线程 exec | 记录临时 ExecSession、late clone 门禁、旧 MM 生命周期、等待点退栈、包装器 fail-closed 裁决及双架构 27/27/初赛门禁 |
| `docs/Work_Log/2026-07-31.md`、同日 B45—B48 evidence | SMP trap context 与 signal 用户访存锁边界 | 记录 trap 借用收口、signal frame 与状态 syscall 的锁外用户访存、Linux ABI 对照、DeepSeek 结论纠错及双架构 8 核初赛门禁 |
| `docs/Work_Log/2026-07-31.md`、`docs/Work_Log/evidence/2026-07-31/smp-b49-work-steal-summary.md` | SMP 空闲核 work stealing | 记录单 victim owner 交接、锁外 kernel-TLB、冻结任务 fail-closed 裁决和双架构 31/31/初赛门禁 |
| `docs/Work_Log/2026-08-01.md`、`docs/Work_Log/evidence/2026-08-01/smp-b50-local-zombie-summary.md` | SMP Per-CPU zombie 回收 | 记录 idle 栈 Arc 交接、跨 CPU reap 锁边界、模型结论纠错及双架构 32/32/初赛门禁 |
| `docs/Work_Log/2026-08-01.md`、`docs/Work_Log/evidence/2026-08-01/smp-b51-active-mm-summary.md` | SMP 精确 active MM 驻留 | 记录 writer/enter/leave 共同 VM 锁、切离屏障、零目标 generation 和双架构 focused/初赛证据 |
| `docs/Work_Log/2026-08-01.md`、`docs/Work_Log/evidence/2026-08-01/smp-b52-range-shootdown-summary.md` | SMP 有界连续用户 TLB shootdown | 记录 64 页 IRQ 上限、双架构 range 后端、固定槽 payload 隔离、65 页全刷与初赛非回归 |
| `docs/Work_Log/2026-08-01.md`、`docs/Work_Log/evidence/2026-08-01/smp-b53-stale-tlb-user-access-summary.md` | SMP 真实用户访存 stale-TLB 证明 | 记录真实 CoW 用户 victim、handler observed-before-ack、假阳性隔离、DeepSeek 首错纠正与双架构 67/67 |

## 9. 交互记录与留痕方式

本项目通过以下方式保留 AI 使用记录：

1. **Git commit metadata**:
   - `Co-authored-by: Copilot <copilot@github.com>`
   - `Ultraworked with Sisyphus (https://github.com/code-yeongyu/oh-my-openagent)`
   - `Co-authored-by: Sisyphus <clio-agent@sisyphuslabs.ai>`
   - commit body 中的 `Oracle analysis`、`Oracle-reviewed`、`Oracle-identified issues`

2. **Development work log**:
   - `docs/Work_Log.md` 持续记录每次重要变更、验证结果和 AI review 结论。

3. **Design / development docs**:
   - 本文件 `docs/00_overview/AI-Usage-Report.md` 作为独立 AI 使用披露。
   - `docs/00_overview/Technical-Report-MangoCore.md`、`docs/00_overview/Engineering-Casebook.md` 等评审文档已通过 Oracle fact-check 修正 AI 生成内容中的不准确点。

4. **Presentation slides**:
   - 最终答辩 slides 应包含"AI 工具使用情况"独立页，摘要可使用本报告第 12 节内容。

## 10. 限制与负面声明

1. 本项目未将 AI 输出作为未经验证的最终事实来源。
2. 本项目未使用 AI 自动绕过测试、伪造测试数据或隐藏失败结果。
3. 本项目未将 AI 生成内容直接作为比赛成果提交，所有代码和文档均经过人工审查。
4. 对于平台未公开或未在 commit metadata 中保存的底层模型版本，本报告不做未经证实的具体模型声明。
5. 文档生成中曾出现事实不准确、虚构抽象或绝对化措辞，已通过 Oracle review 和人工修订进行更正，相关修复记录见 `docs/Work_Log.md:165-265` 和相关 commits。

## 11. 合规自评

| 比赛披露要求 | 本项目对应措施 | 状态 |
|---|---|---|
| 在设计 / 开发文档中声明 AI 工具、模型名称和使用场景 | 本文件第 2 至第 4 节列出工具、模型/版本说明、平台、时间线和使用场景 | 已满足 |
| 在 git commits 中保留 AI 工具产出和交互记录 | 多个 commits 保留 `Co-authored-by: Copilot`、`Co-authored-by: Sisyphus`、`Oracle analysis` 等记录 | 已满足 |
| 在开发文档中设置独立 AI 使用说明 | 本文件为 `docs/00_overview/AI-Usage-Report.md` 独立披露文档 | 已满足 |
| 在设计文档中说明 AI 参与的设计、审查和结果 | 本文件第 4、5、8 节说明架构咨询、设计审查、Work_Log 证据 | 已满足 |
| 在 presentation slides 中设置 AI 工具使用说明 | 最终 slides 应复制或概括第 12 节内容，形成独立"AI 工具使用情况"页 | 待最终 slides 同步 |
| 失败披露视为诚信问题 | 本报告主动披露 AI 工具、使用范围、证据和限制 | 已满足披露要求 |
