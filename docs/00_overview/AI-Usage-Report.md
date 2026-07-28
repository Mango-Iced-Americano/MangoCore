# AI 工具使用情况报告 (AI Tool Usage Report)

> Document path: `docs/00_overview/AI-Usage-Report.md`  
> Project: MangoCore  
> Coverage: 2026-04-01 to 2026-07-28
> Purpose: OS competition AI usage disclosure

## 1. 合规声明

MangoCore 项目在 2026 年 4 月至 2026 年 6 月开发期间使用了多种 AI 工具辅助代码开发、调试、架构审查、性能分析、文档生成与文档事实核查。本报告按照比赛诚信与披露要求，对已使用的 AI 工具、模型名称或平台、使用场景、产出结果、交互记录留痕和人工验证方式进行集中说明。

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
| SMP 本地 TLB 提交边界 | 2026-07-27 | GPT/Codex, DeepSeek | 用户 PTE 写入收口、frame 延迟释放、LA64 ASID 边界审查和双架构 Docker/QEMU 验证 | 建立 `TlbBatch` LocalOnly 协议；RV64/LA64 `CORE_NUM=1 KTEST=mm KREPEAT=2` 均为 8/8 PASS，远端 shootdown 明确 NOT RUN |
| SMP Per-CPU current 槽 | 2026-07-27 | GPT/Codex, DeepSeek | current owner 拆分、Arc/noreturn 生命周期审查、双架构 Docker/QEMU 验证 | 删除全局 PROCESSOR 与 current 裸指针；双架构 `CORE_NUM=4 KTEST=smp KREPEAT=2` 均为 19/19 PASS |
| SMP 初赛非回归门禁 | 2026-07-28 | GPT/Codex, DeepSeek | 双架构 8 核 basic+busybox 执行、judge 失败集合比较、验收规则收敛 | 发现 RV64 8 核 307/314 未达到 312 基线；建立硬条件与只升不降的失败集合门禁 |
| RV64 trap-return 半恢复现场竞态 | 2026-07-28 | GPT/Codex, DeepSeek | 用户 ELF/loader 反汇编、CSR 指令级溯源、双架构 Arc 生命周期复核与 Docker/QEMU 验证 | 统一 `SPP/SIE/SPIE` 返回契约并修复 noreturn Arc 泄漏；RV64 preliminary 312/314、LA64 SMP ktest 10/10 PASS |
| SMP AP 本地调度闭环 | 2026-07-28 | GPT/Codex, DeepSeek | scheduler-ready、AP 页表激活、远程 kernel stack 发布和双架构 8 核验证 | AP 进入本地 scheduler；定位并修复未安装 CPU-local 页表根导致的首次 dispatch 卡死；双架构 23/23 PASS |
| SMP 远程阻塞唤醒 | 2026-07-28 | GPT/Codex, DeepSeek | `last_cpu` 语义、Blocking/Blocked 竞态、批量 wake 锁序与 Docker/QEMU 验证 | AP kernel-only 任务经真实 Completion/WaitQueue 阻塞后回原 CPU；双架构 25/25 PASS |
| SMP kernel-global 撤映射与栈回收 | 2026-07-28 | GPT/Codex, DeepSeek | 全核 TLB sequence/ack、析构延迟回收、双架构 8 核 focused 与初赛回归 | 删除 AP TCB 永久保留 workaround；双架构 27/27 PASS，初赛 RV64 312/314、LA64 308/314，失败集合未扩大 |
| SMP 用户 MM 激活与 user-TLB IPI 基础设施 | 2026-07-28 | GPT/Codex, DeepSeek | VM 锁/ack 死锁审查、MM 驻留与 generation 顺序、独立 user-TLB sequence、双架构 Docker/QEMU 验证 | 保持 `Published` fail-stop，完成激活侧和全用户 IPI/ack 原语；双架构 29/29 PASS，初赛失败集合未扩大，完整 PTE shootdown 明确留给 B23 |

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
- Human action: 建立 `TlbBatch` 和 `Unpublished/LocalOnly/Published` 三态发布边界，收口所有用户 PTE 写入，将失效映射的 frame 延迟到本地 flush 后释放；采纳 LA64 审查项，但拒绝把释放构建的生命周期断言降为 `debug_assert!`。
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
- Problem: B16 的 `TlbBatch` 只有 LocalOnly 语义；直接在其 `commit()` 内加入远端等待会
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
