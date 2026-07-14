# AI 工具使用情况报告 (AI Tool Usage Report)

> Document path: `docs/00_overview/AI-Usage-Report.md`  
> Project: MangoCore  
> Coverage: 2026-04-01 to 2026-07-14
> Purpose: OS competition AI usage disclosure

## 1. 合规声明

MangoCore 项目在 2026 年 4 月至 2026 年 7 月开发期间使用了多种 AI 工具辅助代码开发、调试、架构审查、性能分析、文档生成与文档事实核查。本报告按照比赛诚信与披露要求，对已使用的 AI 工具、模型名称或平台、使用场景、产出结果、交互记录留痕和人工验证方式进行集中说明。

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
| Oracle | 高推理能力代码审查与架构咨询 agent；当前会话模型标识为 GPT-5.5 | OhMyOpenCode agent | 2026-04 至 2026-06 | 根因分析、架构评审、代码正确性验证、性能优化策略、文档事实核查 | `docs/Work_Log.md` 多处记录 `Oracle reviewed`、`Oracle analysis confirmed`、`Root cause analysis by Oracle` |
| Explore | Codebase search / pattern discovery agent | OhMyOpenCode sub-agent | 2026-05 至 2026-06 | 跨模块代码搜索、调用关系梳理、实现模式对比 | Work log 和 Sisyphus task records |
| librarian / plan / deep 等 sub-agents | 专用辅助 agents | OhMyOpenCode sub-agents | 2026-06 | 文档整理、资料检索、复杂任务拆分、局部实现检查 | Sisyphus 编排记录、文档生成 commit、Work_Log 记录 |
| OpenAI Codex multi-agent | 主会话为 GPT-5 系列；2026-07-13 使用 max reasoning mode，平台未单独披露精确后端版本 | Codex desktop | 2026-07 | 2K1000LA 实板 bring-up、LoongArch VALEN/TLB/PTE/DMW、非连续 DRAM 与固件所有权并行审计、代码修复、构建和 QEMU/实板验证 | `docs/Work_Log.md` 2026-07-10/13 记录、目标文件反汇编、uImage 哈希和串口验收日志 |

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
| 2K1000LA 实板地址/TLB 审计 | 2026-07-10 | OpenAI Codex multi-agent | 将 QEMU 内核迁移到 VALEN=40 实板；并行审计 canonical VA、VPN/VPPN、PTE PPN、TLB refill、ASID、DMW 和栈窗口 | 修复 TLB PS、PPN/VPPN、ASID、映射边界和 MMIO 别名；完成双架构编译、LA64 QEMU 用户态启动和实板 uImage 构建 |
| 2K1000LA SATA/FAT32 分阶段写入 | 2026-07-11 | OpenAI Codex | AHCI 暖复位、P2 定向恢复、FAT32 元数据持久化、用户态 `/scratch` 隔离写入与实板串口验证 | 完成 raw write/flush、内核文件探针和用户态 write/fsync/truncate/reopen/unlink/rmdir 闭环；P1/P3 保持只读 |
| 2K1000LA 2 GiB 内存拓扑审计 | 2026-07-13 | OpenAI Codex multi-agent, max reasoning mode | 复核早期扩容方案；并行审计 VA/PA 掩码、DMW cache 属性、U-Boot LMB、DVO DMA、CPU1 park loop 和连续 DMA 分配 | 推翻“DRAM 即已交接”的错误前提；建立双 bank allocator 与临时 carveout，完成跨 bank 320 MiB 压力、QEMU VirtIO/Ext4/LTP 和实板 AHCI 只读验收 |
| 2K1000LA CPython 实机适配 | 2026-07-13 至 2026-07-14 | OpenAI Codex, max reasoning mode | 选择性审计 develop 分支 CPython 链路；对照 QEMU 与实机定位 LSX/FPR 上下文差异，补齐 FAT/TmpFS、外网测试语义和受限 P3 更新工具 | 修复实机 trap 后向量损坏、FAT rename 覆盖和 TmpFS symlink；rv64/la64 QEMU 与 2K1000LA 实板 CPython L3-L9 均 72/72 |

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

2026-07-10 的 2K1000LA 审计使用 Codex multi-agent 将任务拆为掩码/符号扩展、TLB/PTE/CSR、内核栈布局、启动链路和 MMIO/DMW 五个方向。主流程没有直接接受 subagent 结论，而是逐项对照本地《龙芯架构参考手册卷一》、源码、双架构构建、LA64 QEMU 用户态日志和目标文件反汇编后才修改代码。

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

### Case 6: 2K1000LA VALEN=40 与 TLB 全链路审计

- Evidence: `docs/Work_Log.md` 2026-07-10、`docs/09_debug/bug-la64-kernel-stack-overflow.md`
- AI tools: OpenAI Codex multi-agent
- Problem: QEMU 的 48 位高栈窗口迁移到实板后触发 `AddressError`；修正为 40 位 canonical 高栈后，还需确认 VA/VPN/VPPN、PTE、TLB refill、ASID 和 DMW 不受连带影响。
- AI contribution: 五个并行 subagents 独立检查不同硬件语义，主流程汇总后发现 TLB 页大小错误、PTE PPN 掩码过宽、VPPN 裁剪/符号扩展缺失、ASIDBITS 污染和高物理 MMIO VA 属性问题。
- Human verification: 对照本地 LoongArch 官方手册字段定义；检查每处 diff；执行 rv64/la64 编译、LA64 QEMU 到 init 用户态、2K1000 uImage 构建和 `__rfill/__restore` 反汇编。
- Result: 生成 `Load/Entry=0x90000000` 的实板镜像，SHA-256 `e8cf6b87ebd4800f3909fc9aad25d5b7d96957743f5c98fbfd7f7ba4eb8cca78`；实板运行验证仍待完成。

### Case 7: 2K1000LA 2 GiB DRAM 拓扑与固件所有权审计

- Evidence: `docs/Work_Log.md` 2026-07-13、`docs/04_mm/frame-allocator.md`
- AI tools: OpenAI Codex multi-agent, max reasoning mode
- Problem: 初版方案把 U-Boot 报告的两段 DRAM 全部交给页帧分配器，虽然跨 bank 压力短时通过，却可能覆盖仍由显示 DMA、CPU1 和启动固件使用的低端内存。
- AI contribution: 独立 subagents 分别审计 LoongArch VA/PA/DMW 语义和 U-Boot 源码/内存所有权；发现 DMW CC/SUC 探针混用，以及 `[0x0cbf4000,0x10000000)` 内仍包含活动 framebuffer、CPU1 park loop、U-Boot 状态和 BPI/SMBIOS。主流程进一步发现连续 DMA 不能由跨 region 单页分配拼接，且链接器 payload 页需要显式所有权移交。
- Human verification: 对照 U-Boot `bdinfo`、板级 U-Boot 源码和串口输出；串行双架构构建；LA64 QEMU VirtIO/Ext4/LTP 运行；实板 320 MiB 跨 bank 内容校验、AHCI LBA0 重复读和 ABI 内存统计检查。
- Result: 内核识别完整 2 GiB 安装容量，当前安全报告并使用 `2043852 KiB`；保留 53,296 KiB 临时 carveout，待关闭 DVO、重停放 CPU1 并处理启动参数后再分阶段释放。

### Case 8: 2K1000LA CPython 的实机 LSX/FPR 上下文损坏

- Evidence: `docs/Work_Log.md` 2026-07-14、`os/src/hal/arch/loongarch64/trap/trap.S`、`os/src/syscall/process/signal.rs`、`os/src/fs/fat32/{efs.rs,fat_inode.rs}`、`os/src/fs/page_cache.rs`、`scripts/write_2k1000_p3.py`
- AI tools: OpenAI Codex, max reasoning mode
- Problem: Alpine LoongArch CPython 在 QEMU 可运行，实板却在 syscall、定时器或调度后出现动态运行时数据损坏；单次启动位置不固定，容易误判为 ELF、内存或 CPython 本身问题。
- AI contribution: 对比 QEMU 与实机 CPU 扩展行为，审计 trap/save-restore 和 signal frame 后识别到标量 FPR 与 LSX 向量低 64-bit lane 的物理别名。旧汇编先恢复完整 LSX、随后执行标量 `FLD.D`，会在实机重新覆盖向量状态，而 QEMU 未可靠暴露该行为；随后根据 FAT 旧 payload 证据定位 inode/PageCache 生命周期，并生成固定边界、逐块读回的 P3 更新工具。
- Human verification: 审阅汇编、signal ABI、FAT inode/PageCache 生命周期和写盘边界；Docker 串行双架构构建；rv64/la64 CPython L3-L9 QEMU judge 各 72/72；2K1000LA 通过 50 轮无 `fsync` rename 专项，P3 三块写入/读回 CRC 和安装文件校验，最终完整 L3-L9 同样为 72/72、退出码 0。
- Result: trap 返回在完整 LSX 与纯标量 FPR 恢复路径中二选一，`sigreturn` 先合并标量低 lane；FAT 以首簇/空目录项双键 canonicalize inode，并让 PageCache 共享最小簇链状态，从根因修复 Drop 写回丢失；TmpFS、DNS/HTTP/HTTPS 和实板 CPython 完整组合门禁均已关闭。

### Case 9: 2K1000LA Python 性能的 DMA 与字节码分层定位

- Evidence: `docs/Work_Log.md` 2026-07-14、`docs/07_driver/2k1000-ahci.md`
- AI tools: OpenAI Codex, max reasoning mode
- Problem: Python 和其他 SSD 程序明显偏慢，初始假设是 AHCI 以过小 DMA 块搬运；需要先保存实板基线，再判断队友 VirtIO DMA 池化方案应如何迁移。
- AI contribution: 对照 develop 的四槽 VirtIO 池、当前 PageCache 批量请求和 AHCI 单命令路径，发现 256 KiB 上层请求被重新拆成最多 512 条轮询 ATA 命令。根据 AHCI 已由互斥串行化这一事实，将方案收敛为单个常驻 64 KiB 连续低端槽和多扇区命令，而非机械移植多槽状态机。随后用 tmpfs 和 real/user/sys 拆分证明重导入的最大剩余成本是只读标准库禁用 pyc 后的用户态重复解析/编译。
- Human verification: 在同一 2K1000LA/SSD 上记录 512 B、64 KiB、256 KiB 三版数据；执行双架构编译和 QEMU LTP 冒烟；实板完成 TFTP/uImage 校验、随机大块写入、哈希/`cmp`、P4 pyc 首次填充/稳定命中和显式同步后的复位门禁。
- Result: 首次顺序读由 13.5 MB/s 升到 18.6 MB/s；Python 无 site 热启动由约 1.925 s 降到 1.714 s。外置持久 pyc 后进一步降到约 1.159 s，重模块导入由 18.322 s 降到约 4.495 s；256 KiB 槽无额外收益，最终保留 64 KiB。

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
| `docs/Work_Log.md` 2026-07-10 | 2K1000LA VALEN/TLB 审计 | 记录 Codex 五路并行审计、官方手册交叉核对、代码修复、反汇编与构建/QEMU 证据 |
| `docs/Work_Log.md` 2026-07-13 | 2K1000LA 2 GiB 内存审计 | 记录 Codex max reasoning 与 subagent 对 DMW、非连续 DMA、U-Boot/DVO/CPU1 所有权的复核，以及 QEMU/实板验证证据 |
| `docs/Work_Log.md` 2026-07-14 | 2K1000LA CPython 实机适配 | 记录 Codex 对 LSX/FPR 别名、FAT rename、TmpFS symlink 和 DNS/HTTPS 测试语义的根因分析，以及双架构 72/72 与实机压力证据 |
| `docs/Work_Log.md` 2026-07-14 | 2K1000LA AHCI/Python 性能 | 记录 Codex 对 develop DMA 池化方案的并发模型审计、512 B 命令放大根因、64/256 KiB 实板 A/B，以及 pyc 用户态瓶颈分层证据 |

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
